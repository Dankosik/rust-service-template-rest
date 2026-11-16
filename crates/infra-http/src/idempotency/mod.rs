mod compose;
mod declaration;
mod execute;
mod identity;
mod openapi;
mod stored;

pub use compose::{Activation, Composer};
pub use declaration::CompositionError;
pub use execute::{HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Idempotency};
pub use infra_postgres::Tx;
pub(crate) use openapi::declares_replay;

const KEY_HEADER: &str = "Idempotency-Key";
const REPLAYED_HEADER: &str = "Idempotent-Replayed";
const REPLAYED_VALUE: &str = "true";

fn caused_by_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::causes(error)
        .any(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
}
