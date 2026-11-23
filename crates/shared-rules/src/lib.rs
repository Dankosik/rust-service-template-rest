//! A rule belongs here only when all of these hold:
//!
//! - It is one piece of knowledge: changing it in one caller would have to
//!   change it in every caller. A standard (an RFC, the OpenTelemetry
//!   conventions), the way platforms write mounted secret files, the
//!   standard library, or one formula fixes it.
//! - At least two crates apply it and share no owner that could hold it. A
//!   rule from an external standard joins with its second caller; a formula
//!   of the service's own joins with its third.
//! - It is pure: no I/O, clock, randomness, global state, or configuration.
//! - It needs nothing beyond `std`; this crate declares no dependencies.
//! - It returns plain data: values, or a closed set of outcomes. Each caller
//!   keeps its own policy (windows, random sources, error codes and wording)
//!   and maps the result into its own error.

// template:begin cache:shared-rules-cache
pub mod cache {
    pub const MAX_ENTRY_BYTES: usize = 512 * 1024 * 1024;
}
// template:end cache:shared-rules-cache
// template:begin clickhouse:shared-rules-clickhouse
pub mod clickhouse;
// template:end clickhouse:shared-rules-clickhouse
pub mod error_chain;
// template:begin grpc:shared-rules-grpc
pub mod grpc;
// template:end grpc:shared-rules-grpc
pub mod hyper {
    pub const HTTP1_MIN_READ_BUFFER_BYTES: u16 = 8 * 1024;
}
pub mod jitter;
// template:begin messaging:shared-rules-messaging
pub mod messaging {
    pub const HEADER_LIMIT_BYTES: usize = 8 * 1024;
    pub const PUBLICATION_WINDOW_BYTES: usize = 64 * 1024 * 1024;
}
// template:end messaging:shared-rules-messaging
// template:begin oidc-jwt:jwks-freshness-policy
pub mod oidc_jwt {
    use std::time::Duration;

    pub const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
    pub const MIN_JWKS_MAX_AGE: Duration = REFRESH_COOLDOWN.saturating_mul(2);
}
// template:end oidc-jwt:jwks-freshness-policy
// template:begin oidc-introspection:introspection-cache-ranges
pub mod oidc_introspection {
    use std::{ops::RangeInclusive, time::Duration};

    pub const CACHE_CAPACITY: RangeInclusive<usize> = 1..=1024;
    pub const CACHE_TTL: RangeInclusive<Duration> = Duration::from_secs(1)..=Duration::from_mins(5);
}
// template:end oidc-introspection:introspection-cache-ranges
pub mod otel_http;
pub mod rfc6749;
pub mod rfc6750;
pub mod rfc6901;
pub mod rfc7515;
pub mod rfc7518;
pub mod rfc9110;
pub mod secret_file;
pub mod uri;
