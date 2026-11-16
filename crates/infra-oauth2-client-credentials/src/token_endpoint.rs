use std::time::Duration;

use bytes::Bytes;
use http::{
    Request, Response, StatusCode, Uri,
    header::{ACCEPT, CONTENT_TYPE},
};
use infra_outbound_http::{Client, Limits};
use operation_context::{Deadline, OperationContext};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Deserializer};
use tokio::{
    sync::{Semaphore, SemaphorePermit},
    time::Instant,
};
use url::{Url, form_urlencoded::Serializer};

use crate::{AcquisitionError, ConfigurationError, Rejection, assertion::Signer, token::Token};

pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const TOKEN_LIMITS: Limits = Limits {
    operation_timeout: FETCH_TIMEOUT,
    response_header_count: 64,
    response_body_bytes: 1024 * 1024,
};
const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";
const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
const ACQUISITIONS_TOTAL: &str = "oauth2_token_acquisitions_total";

/// The authorization server's token endpoint as both grants reach it: one
/// signed, bounded, counted request per attempt, with no reuse of its own.
pub(crate) struct TokenEndpoint {
    uri: Uri,
    http: Client,
    scope: Option<String>,
    audience: Option<String>,
    signer: Signer,
    permits: Semaphore,
}

impl TokenEndpoint {
    pub(crate) fn new(
        url: Url,
        http: Client,
        scopes: &[String],
        audience: Option<String>,
        signer: Signer,
        permits: usize,
    ) -> Result<Self, ConfigurationError> {
        let uri = Uri::try_from(String::from(url)).map_err(|_| ConfigurationError {
            key: "token_url",
            reason: "must be a valid HTTPS URL",
        })?;
        metrics::describe_counter!(
            ACQUISITIONS_TOTAL,
            "OAuth2 token attempts by grant and closed terminal outcome"
        );
        Ok(Self {
            uri,
            http,
            scope: joined_scopes(scopes),
            audience,
            signer,
            permits: Semaphore::new(permits),
        })
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.uri.host()
    }

    /// The caller includes the [`FETCH_TIMEOUT`] cap in `deadline`.
    pub(crate) async fn fetch_service_token(
        &self,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        self.fetch_token(Grant::ClientCredentials, deadline).await
    }

    /// Only the caller that owns the active exchange for the subject runs this attempt.
    pub(crate) async fn fetch_exchange(
        &self,
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        self.fetch_token(Grant::TokenExchange { subject }, deadline)
            .await
    }

    async fn fetch_token(
        &self,
        grant: Grant<'_>,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let started = Instant::now();
        let metric = std::mem::DropGuard::new(grant.metric_label(), |grant| {
            let outcome = if Instant::now() >= deadline {
                AcquisitionError::Timeout.label()
            } else {
                AcquisitionError::Cancelled.label()
            };
            record_acquisition(grant, outcome);
        });
        let result = tokio::time::timeout_at(deadline, async {
            let _permit = self.admit_attempt(deadline)?;
            self.request_token(grant, started, deadline).await
        })
        .await
        .unwrap_or(Err(AcquisitionError::Timeout));
        // Synchronous decoding and token admission can finish after the timer's
        // last poll.
        let result = if Instant::now() >= deadline {
            Err(AcquisitionError::Timeout)
        } else {
            result
        };
        let grant = std::mem::DropGuard::dismiss(metric);
        record_acquisition(
            grant,
            result
                .as_ref()
                .err()
                .copied()
                .map_or("success", AcquisitionError::label),
        );
        result
    }

    fn admit_attempt(&self, deadline: Instant) -> Result<SemaphorePermit<'_>, AcquisitionError> {
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        self.permits
            .try_acquire()
            .map_err(|_| AcquisitionError::AtCapacity)
    }

    async fn request_token(
        &self,
        grant: Grant<'_>,
        started: Instant,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let body = self.form_body(grant)?;
        let response = self.post_form(body, deadline).await?;
        token_from_response(&response, started, grant.expected_issued_type())
    }

    fn form_body(&self, grant: Grant<'_>) -> Result<String, AcquisitionError> {
        let assertion = self.signer.sign()?;
        let mut form = Serializer::new(String::new());
        grant.append_fields(&mut form);
        if let Some(scope) = self.scope.as_deref() {
            form.append_pair("scope", scope);
        }
        if let Some(audience) = self.audience.as_deref() {
            form.append_pair("audience", audience);
        }
        Ok(form
            .extend_pairs(self.signer.form_fields(&assertion))
            .finish())
    }

    async fn post_form(
        &self,
        body: String,
        deadline: Instant,
    ) -> Result<TokenResponse, AcquisitionError> {
        let request = Request::post(self.uri.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(Bytes::from(body))
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        let mut observed_status = None;
        let context = OperationContext::from_deadline(Deadline::at(deadline));
        let response = self
            .http
            .execute_with_response_status(request, &context, |status| {
                observed_status = Some(status);
            })
            .await;
        classify_token_response(response, observed_status)
    }
}

#[derive(Clone, Copy)]
enum Grant<'subject> {
    ClientCredentials,
    TokenExchange { subject: &'subject SecretString },
}

impl Grant<'_> {
    fn append_fields(self, form: &mut Serializer<'_, String>) {
        match self {
            Self::ClientCredentials => form.append_pair("grant_type", GRANT_CLIENT_CREDENTIALS),
            Self::TokenExchange { subject } => form
                .append_pair("grant_type", GRANT_TOKEN_EXCHANGE)
                .append_pair("subject_token", subject.expose_secret())
                .append_pair("subject_token_type", TOKEN_TYPE_ACCESS_TOKEN)
                .append_pair("requested_token_type", TOKEN_TYPE_ACCESS_TOKEN),
        };
    }

    fn metric_label(self) -> &'static str {
        match self {
            Self::ClientCredentials => "client_credentials",
            Self::TokenExchange { .. } => "token_exchange",
        }
    }

    fn expected_issued_type(self) -> Option<&'static str> {
        match self {
            Self::ClientCredentials => None,
            Self::TokenExchange { .. } => Some(TOKEN_TYPE_ACCESS_TOKEN),
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: zeroize::Zeroizing<String>,
    token_type: String,
    #[serde(default, deserialize_with = "deserialize_lifetime")]
    expires_in: Option<u64>,
    issued_token_type: Option<String>,
}

fn deserialize_lifetime<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}

fn classify_token_response(
    response: Result<Response<Bytes>, infra_outbound_http::Error>,
    observed_status: Option<StatusCode>,
) -> Result<TokenResponse, AcquisitionError> {
    use infra_outbound_http::Error::{ResponseBodyTooLarge, Transport};

    match (response, observed_status) {
        (Ok(response), _) if response.status().is_success() => decode_token_response(&response),
        (Ok(response), _) => Err(provider_failure(
            response.status(),
            Some(response.body().as_ref()),
        )),
        (Err(ResponseBodyTooLarge | Transport { .. }), Some(status)) if !status.is_success() => {
            Err(provider_failure(status, None))
        }
        (Err(error), _) => Err(map_transport_error(&error)),
    }
}

fn provider_failure(status: StatusCode, body: Option<&[u8]>) -> AcquisitionError {
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return AcquisitionError::Unavailable;
    }
    AcquisitionError::Rejected(body.map_or(Rejection::Other, Rejection::from_body))
}

fn decode_token_response(response: &Response<Bytes>) -> Result<TokenResponse, AcquisitionError> {
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let value = content_type
            .to_str()
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        if !shared_rules::rfc9110::is_json_content_type(value) {
            return Err(AcquisitionError::InvalidResponse);
        }
    }
    serde_json::from_slice(response.body().as_ref()).map_err(|_| AcquisitionError::InvalidResponse)
}

fn map_transport_error(error: &infra_outbound_http::Error) -> AcquisitionError {
    match error {
        infra_outbound_http::Error::Timeout => AcquisitionError::Timeout,
        infra_outbound_http::Error::Cancelled => AcquisitionError::Cancelled,
        infra_outbound_http::Error::ResponseBodyTooLarge => AcquisitionError::ResponseLimit,
        infra_outbound_http::Error::InvalidTarget
        | infra_outbound_http::Error::DispatchRefused
        | infra_outbound_http::Error::Transport { .. } => AcquisitionError::Transport,
    }
}

fn joined_scopes(scopes: &[String]) -> Option<String> {
    if scopes.is_empty() {
        None
    } else {
        Some(scopes.join(" "))
    }
}

/// When `expected_issued_token_type` is set, admission requires an exact
/// match with the provider's issued type (RFC 8693 section 2.2.1).
fn token_from_response(
    response: &TokenResponse,
    started: Instant,
    expected_issued_token_type: Option<&str>,
) -> Result<Token, AcquisitionError> {
    if !response.token_type.eq_ignore_ascii_case("bearer") {
        return Err(AcquisitionError::InvalidResponse);
    }
    if let Some(expected) = expected_issued_token_type
        && response.issued_token_type.as_deref() != Some(expected)
    {
        return Err(AcquisitionError::InvalidResponse);
    }
    Token::bearer(&response.access_token, started, response.expires_in)
}

fn record_acquisition(grant: &'static str, outcome: &'static str) {
    metrics::counter!(
        ACQUISITIONS_TOTAL,
        "grant" => grant,
        "outcome" => outcome,
    )
    .increment(1);
}
