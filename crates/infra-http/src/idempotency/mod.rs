mod compose;
mod declaration;
mod execute;
mod identity;
mod openapi;
mod stored;

pub use compose::{Activation, Composer};
pub use declaration::CompositionError;
pub use execute::{HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Idempotency, Rollback};
pub use infra_postgres::Tx;
pub(crate) use openapi::declares_replay;
pub use stored::ReplayableResponse;

const KEY_HEADER: &str = "Idempotency-Key";
const REPLAYED_HEADER: &str = "Idempotent-Replayed";
const REPLAYED_VALUE: &str = "true";
