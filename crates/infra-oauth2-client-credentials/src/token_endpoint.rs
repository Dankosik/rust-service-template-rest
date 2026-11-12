use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http::{
    Request, Response, StatusCode,
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
use url::Url;

use crate::{AcquisitionError, Rejection, assertion::Signer, token::Token};

pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const TOKEN_LIMITS: Limits = Limits {
    operation_timeout: FETCH_TIMEOUT,
    response_header_count: 64,
    response_body_bytes: 1024 * 1024,
};
const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";
const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
/// The token-attempt metric identity, shared by its description and records.
const ACQUISITIONS_TOTAL: &str = "oauth2_token_acquisitions_total";

/// The authorization server's token endpoint as both grants reach it: one
/// signed, bounded, counted request per attempt, with no reuse of its own.
pub(crate) struct TokenEndpoint {
    url: Url,
    http: Client,
    scopes: Vec<String>,
    audience: Option<String>,
    signer: Signer,
    permits: Semaphore,
}

impl TokenEndpoint {
    pub(crate) fn new(
        url: Url,
        http: Client,
        scopes: Vec<String>,
        audience: Option<String>,
        signer: Signer,
        permits: usize,
    ) -> Self {
        metrics::describe_counter!(
            ACQUISITIONS_TOTAL,
            "OAuth2 token attempts by grant and closed terminal outcome"
        );
        Self {
            url,
            http,
            scopes,
            audience,
            signer,
            permits: Semaphore::new(permits),
        }
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.url.host_str()
    }

    /// Performs one client-credentials token request, bounded by the
    /// caller's deadline and [`FETCH_TIMEOUT`], and records its outcome.
    pub(crate) async fn fetch_service_token(
        &self,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let started = Instant::now();
        let grant = Grant::ClientCredentials;
        let mut metric = AttemptMetric::new(grant.metric_label(), deadline);
        let result = tokio::time::timeout_at(deadline, async {
            let _permit = self.admit_attempt(deadline)?;
            self.request_token(grant, started, deadline).await
        })
        .await
        .unwrap_or(Err(AcquisitionError::Timeout));
        let result = if Instant::now() >= deadline {
            Err(AcquisitionError::Timeout)
        } else {
            result
        };

        metric.finish(&result);
        result
    }

    /// Performs one token-exchange request for `subject`, bounded by
    /// `deadline`, and records its outcome. Called only as the initializer
    /// of [`Credentials::exchanged_or_fetch`](crate::Credentials::exchanged_or_fetch)'s
    /// `or_insert_with_if`, so it runs at most once per coalesced group of
    /// concurrent callers for one subject.
    pub(crate) async fn fetch_exchange(
        &self,
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let started = Instant::now();
        let grant = Grant::TokenExchange { subject };
        let mut metric = AttemptMetric::new(grant.metric_label(), deadline);
        let result = async {
            let _permit = self.admit_attempt(deadline)?;
            self.request_token(grant, started, deadline).await
        }
        .await;
        metric.finish(&result);
        result.map(Arc::new)
    }

    fn admit_attempt(&self, deadline: Instant) -> Result<SemaphorePermit<'_>, AcquisitionError> {
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        self.permits
            .try_acquire()
            .map_err(|_| AcquisitionError::AtCapacity)
    }

    /// Sends one token request for `grant`: its own parameters, then scope
    /// and audience, then client authentication, the order RFC 7523's
    /// example uses.
    async fn request_token(
        &self,
        grant: Grant<'_>,
        started: Instant,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let assertion = self.signer.sign()?;
        let scope = joined_scopes(&self.scopes);
        let mut fields = match grant {
            Grant::ClientCredentials => vec![("grant_type", GRANT_CLIENT_CREDENTIALS)],
            Grant::TokenExchange { subject } => vec![
                ("grant_type", GRANT_TOKEN_EXCHANGE),
                ("subject_token", subject.expose_secret()),
                ("subject_token_type", TOKEN_TYPE_ACCESS_TOKEN),
                ("requested_token_type", TOKEN_TYPE_ACCESS_TOKEN),
            ],
        };
        if let Some(scope) = scope.as_deref() {
            fields.push(("scope", scope));
        }
        if let Some(audience) = self.audience.as_deref() {
            fields.push(("audience", audience));
        }
        fields.extend(self.signer.form_fields(&assertion));
        let response = self.post_form(&fields, deadline).await?;
        token_from_response(&response, started, grant.expected_issued_type())
    }

    async fn post_form(
        &self,
        fields: &[(&str, &str)],
        deadline: Instant,
    ) -> Result<TokenResponse, AcquisitionError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let request = Request::post(self.url.as_str())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(Bytes::from(body))
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        let mut provider_unavailable = false;
        let context = OperationContext::from_deadline(Deadline::at(deadline));
        let response = self
            .http
            .execute_with_response_status(request, &context, |status| {
                provider_unavailable =
                    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
            })
            .await;
        if provider_unavailable
            && matches!(
                &response,
                Ok(_)
                    | Err(infra_outbound_http::Error::ResponseBodyTooLarge
                        | infra_outbound_http::Error::Transport { .. })
            )
        {
            return Err(AcquisitionError::Unavailable);
        }
        let response = response.map_err(|error| map_transport_error(&error))?;
        decode_token_response(&response)
    }
}

/// One token grant: what its request sends and how its answer is admitted.
#[derive(Clone, Copy)]
enum Grant<'a> {
    ClientCredentials,
    TokenExchange { subject: &'a SecretString },
}

impl Grant<'_> {
    /// The `grant` metric label; shorter than the URN token exchange sends as
    /// `grant_type`.
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
    access_token: String,
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

fn decode_token_response(response: &Response<Bytes>) -> Result<TokenResponse, AcquisitionError> {
    let status = response.status();
    if !status.is_success() {
        return Err(AcquisitionError::Rejected(Rejection::from_body(
            response.body(),
        )));
    }
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

/// Admits a parsed response as a [`Token`]. With `issued_token_type` set, a response is
/// admitted only when it reports that exact issued type (RFC 8693 section
/// 2.2.1).
fn token_from_response(
    response: &TokenResponse,
    started: Instant,
    issued_token_type: Option<&str>,
) -> Result<Token, AcquisitionError> {
    if !response.token_type.eq_ignore_ascii_case("bearer") {
        return Err(AcquisitionError::InvalidResponse);
    }
    if let Some(expected) = issued_token_type
        && response.issued_token_type.as_deref() != Some(expected)
    {
        return Err(AcquisitionError::InvalidResponse);
    }
    Token::bearer(&response.access_token, started, response.expires_in)
}

struct AttemptMetric {
    grant: &'static str,
    outcome: Option<&'static str>,
    deadline: Instant,
}

impl AttemptMetric {
    fn new(grant: &'static str, deadline: Instant) -> Self {
        Self {
            grant,
            outcome: None,
            deadline,
        }
    }

    fn finish<T>(&mut self, result: &Result<T, AcquisitionError>) {
        self.outcome = Some(
            result
                .as_ref()
                .err()
                .map_or("success", |error| error.label()),
        );
    }
}

impl Drop for AttemptMetric {
    fn drop(&mut self) {
        let outcome = self.outcome.unwrap_or(if Instant::now() >= self.deadline {
            AcquisitionError::Timeout.label()
        } else {
            AcquisitionError::Cancelled.label()
        });
        metrics::counter!(
            ACQUISITIONS_TOTAL,
            "grant" => self.grant,
            "outcome" => outcome,
        )
        .increment(1);
    }
}
