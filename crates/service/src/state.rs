use axum::extract::FromRef;
use health::ReadinessReader;
// template:begin inbound-webhooks:service-state-webhooks-import
use infra_http::webhooks::WebhookState;
// template:end inbound-webhooks:service-state-webhooks-import

#[derive(Clone, Debug, FromRef)]
pub struct AppState {
    pub readiness: ReadinessReader,
    // template:begin inbound-webhooks:service-state-webhooks-field
    pub webhooks: WebhookState,
    // template:end inbound-webhooks:service-state-webhooks-field
    // template:begin service-auth:service-state-service-tokens-field
    pub service_tokens: infra_serviceauth::TokenSource,
    // template:end service-auth:service-state-service-tokens-field
}
