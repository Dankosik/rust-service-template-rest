use axum::extract::FromRef;
use health::ReadinessReader;
// template:begin inbound-webhooks:service-state-webhooks-import
use infra_http::webhooks::WebhookState;
// template:end inbound-webhooks:service-state-webhooks-import

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
