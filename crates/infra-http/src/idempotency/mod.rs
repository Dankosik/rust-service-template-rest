mod compose;
mod declaration;
mod execute;
mod identity;
mod input;
mod openapi;
mod stored;

#[macro_export]
macro_rules! idempotent_route {
    ($($handler:ident)::+) => {{
        #[derive(utoipa::OpenApi)]
        #[openapi(paths($($handler)::+))]
        struct Document;

        $crate::idempotency::route(<Document as utoipa::OpenApi>::openapi(), $($handler)::+)
    }};
}

pub use compose::route;
pub use declaration::CompositionError;
pub use execute::{FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Rollback};
pub use infra_postgres::Tx;
pub use input::CommandInput;
pub use stored::ReplayableResponse;

pub(crate) const KEY_HEADER: &str = "Idempotency-Key";
pub(crate) const REPLAYED_HEADER: &str = "Idempotent-Replayed";
const REPLAYED_VALUE: &str = "true";
