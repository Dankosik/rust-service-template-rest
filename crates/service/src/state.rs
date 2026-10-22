//! The state every route of the service shares.
//!
//! A router names the part it needs (`ReadinessReader: FromRef<S>`), and
//! axum hands a handler that part through `State`, so a route whose state the
//! composition root does not supply fails to compile instead of answering
//! `500`. A feature adds its own state type as one more field, built in
//! bootstrap from the dependencies it opened.

use axum::extract::FromRef;
use health::ReadinessReader;
// template:begin inbound-webhooks:service-state-webhooks-import
use infra_http::webhooks::WebhookState;
// template:end inbound-webhooks:service-state-webhooks-import

/// Application state, applied to the finalized routes once in bootstrap.
#[derive(Clone, Debug)]
pub struct AppState {
    pub readiness: ReadinessReader,
    // template:begin inbound-webhooks:service-state-webhooks-field
    pub webhooks: WebhookState,
    // template:end inbound-webhooks:service-state-webhooks-field
    // template:begin service-auth:service-state-service-tokens-field
    /// This service's outbound token source: one handle per receiver,
    /// `TokenSource::receiver("catalog")`, for gRPC and HTTP clients.
    pub service_tokens: infra_serviceauth::TokenSource,
    // template:end service-auth:service-state-service-tokens-field
}

impl FromRef<AppState> for ReadinessReader {
    fn from_ref(state: &AppState) -> Self {
        state.readiness.clone()
    }
}
// template:begin inbound-webhooks:service-state-webhooks-from-ref

impl FromRef<AppState> for WebhookState {
    fn from_ref(state: &AppState) -> Self {
        state.webhooks.clone()
    }
}
// template:end inbound-webhooks:service-state-webhooks-from-ref
// template:begin service-auth:service-state-service-tokens-from-ref

impl FromRef<AppState> for infra_serviceauth::TokenSource {
    fn from_ref(state: &AppState) -> Self {
        state.service_tokens.clone()
    }
}
// template:end service-auth:service-state-service-tokens-from-ref
