use axum::extract::FromRef;
use health::ReadinessReader;
// template:begin inbound-webhooks:service-state-webhooks-import
use infra_webhooks::inbound::route::WebhookState;
// template:end inbound-webhooks:service-state-webhooks-import

#[derive(Clone, Debug, FromRef)]
pub struct AppState {
    pub readiness: ReadinessReader,
    // template:begin inbound-webhooks:service-state-webhooks-field
    pub webhooks: WebhookState,
    // template:end inbound-webhooks:service-state-webhooks-field
    // template:begin webhooks:service-state-outbound-webhooks-field
    pub outbound_webhooks: infra_webhooks::outbound::Producer,
    // template:end webhooks:service-state-outbound-webhooks-field
    // template:begin service-auth:service-state-service-tokens-field
    pub service_tokens: infra_serviceauth::TokenSource,
    // template:end service-auth:service-state-service-tokens-field
    pub temporal: Option<infra_temporal::TemporalClient>,
}
