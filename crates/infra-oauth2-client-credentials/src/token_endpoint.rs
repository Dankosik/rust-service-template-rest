use std::time::Duration;

use bytes::Bytes;
use http::{
    Request, Response, StatusCode, Uri,
    header::{ACCEPT, CONTENT_TYPE},
};
use infra_outbound_http::{Client, ExchangeError, Limits, RequestContext};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Deserializer};
use tokio::{sync::Semaphore, time::Instant};
use url::{Url, form_urlencoded::Serializer};

use crate::{
    AcquisitionError, ConfigurationError, ExchangeTokenTarget, Rejection, ServiceTokenTarget,
    assertion::Signer, token::Token,
};

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

pub(crate) struct TokenEndpoint {
    uri: Uri,
    http: Client,
    signer: Signer,
    permits: Semaphore,
}

impl TokenEndpoint {
    pub(crate) fn new(
        url: Url,
        http: Client,
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
            signer,
            permits: Semaphore::new(permits),
        })
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.uri.host()
    }

    pub(crate) async fn fetch_service_token(
        &self,
        target: &ServiceTokenTarget,
        context: &RequestContext,
    ) -> Result<Token, AcquisitionError> {
        self.fetch_token(Grant::ClientCredentials(target), context)
            .await
    }

    pub(crate) async fn fetch_exchange(
        &self,
        subject: &SecretString,
        target: &ExchangeTokenTarget,
        context: &RequestContext,
    ) -> Result<Token, AcquisitionError> {
        self.fetch_token(Grant::TokenExchange { subject, target }, context)
            .await
    }

    async fn fetch_token(
        &self,
        grant: Grant<'_>,
        context: &RequestContext,
    ) -> Result<Token, AcquisitionError> {
        let started = Instant::now();
        let check = || context.check().map_err(AcquisitionError::from);
        let metric = std::mem::DropGuard::new(grant.metric_label(), |grant_label| {
            let error = check().err().unwrap_or(AcquisitionError::Cancelled);
            record_acquisition(grant_label, error.label());
        });
        let result = match check() {
            Ok(()) => self.request_token(grant, started, context).await,
            Err(error) => Err(error),
        };
        let result = check().and(result);
        let grant_label = std::mem::DropGuard::dismiss(metric);
        record_acquisition(
            grant_label,
            result
                .as_ref()
                .err()
                .copied()
                .map_or("success", AcquisitionError::label),
        );
        result
    }

    async fn request_token(
        &self,
        grant: Grant<'_>,
        started: Instant,
        context: &RequestContext,
    ) -> Result<Token, AcquisitionError> {
        let _permit = self
            .permits
            .try_acquire()
            .map_err(|_| AcquisitionError::AtCapacity)?;
        let request = self.token_request(grant)?;
        let exchange = self.http.execute_buffered(request, context.clone()).await;
        let response = classify_token_response(exchange)?;
        token_from_response(&response, started, grant.expected_issued_type())
    }

    fn token_request(&self, grant: Grant<'_>) -> Result<Request<Bytes>, AcquisitionError> {
        let body = {
            let assertion = self.signer.sign()?;
            let mut form = Serializer::new(String::new());
            grant.append_fields(&mut form);
            form.extend_pairs(self.signer.form_fields(&assertion))
                .finish()
        };
        Request::post(self.uri.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(Bytes::from(body))
            .map_err(|_| AcquisitionError::InvalidResponse)
    }
}

#[derive(Clone, Copy)]
enum Grant<'a> {
    ClientCredentials(&'a ServiceTokenTarget),
    TokenExchange {
        subject: &'a SecretString,
        target: &'a ExchangeTokenTarget,
    },
}

impl Grant<'_> {
    fn append_fields(self, form: &mut Serializer<'_, String>) {
        match self {
            Self::ClientCredentials(target) => form
                .append_pair("grant_type", GRANT_CLIENT_CREDENTIALS)
                .extend_pairs(target.scope.as_deref().map(|scope| ("scope", scope))),
            Self::TokenExchange { subject, target } => form
                .append_pair("grant_type", GRANT_TOKEN_EXCHANGE)
                .append_pair("subject_token", subject.expose_secret())
                .append_pair("subject_token_type", TOKEN_TYPE_ACCESS_TOKEN)
                .append_pair("requested_token_type", TOKEN_TYPE_ACCESS_TOKEN)
                .extend_pairs(target.scope.as_deref().map(|scope| ("scope", scope)))
                .extend_pairs(
                    target
                        .audience
                        .as_deref()
                        .map(|audience| ("audience", audience)),
                ),
        };
    }

    fn metric_label(self) -> &'static str {
        match self {
            Self::ClientCredentials(_) => "client_credentials",
            Self::TokenExchange { .. } => "token_exchange",
        }
    }

    fn expected_issued_type(self) -> Option<&'static str> {
        match self {
            Self::ClientCredentials(_) => None,
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
    response: Result<Response<Bytes>, ExchangeError>,
) -> Result<TokenResponse, AcquisitionError> {
    use infra_outbound_http::Error::{ResponseBodyTooLarge, Transport};

    match response {
        Ok(response) if response.status() == StatusCode::OK => decode_token_response(&response),
        Ok(response) => Err(provider_failure(
            response.status(),
            Some(response.body().as_ref()),
        )),
        Err(ExchangeError::AfterResponse {
            status,
            source: ResponseBodyTooLarge | Transport { .. },
        }) if status != StatusCode::OK => Err(provider_failure(status, None)),
        Err(error) => Err(crate::TransportFailure::from(error.into_error()).into()),
    }
}

fn provider_failure(status: StatusCode, body: Option<&[u8]>) -> AcquisitionError {
    if status.is_success() {
        return AcquisitionError::InvalidResponse;
    }
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

/// RFC 8693 §2.2.1
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
