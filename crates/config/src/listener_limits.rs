use std::time::Duration;

use crate::validate::{ValidationError, duration_range, int_range};

pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
pub(super) const DEFAULT_MAX_IN_FLIGHT: u32 = 256;
// Keep headroom for request shedding to return an informative response.
// At the connection cap, the listener closes the socket without responding.
pub(super) const DEFAULT_MAX_CONNECTIONS: u32 = 4096;

pub(super) fn request_timeout_range(key: &str, value: Duration) -> Result<(), ValidationError> {
    duration_range(
        key,
        value,
        Duration::from_millis(100),
        Duration::from_secs(600),
    )
}

pub(super) fn in_flight_range(key: &str, value: u32) -> Result<(), ValidationError> {
    int_range(key, u64::from(value), 0, 100_000)
}

pub(super) fn connection_count_range(key: &str, value: u32) -> Result<(), ValidationError> {
    int_range(key, u64::from(value), 0, 1_000_000)
}

pub(super) fn connection_age_range(key: &str, value: Duration) -> Result<(), ValidationError> {
    if value.is_zero() {
        return Ok(());
    }
    duration_range(key, value, Duration::from_secs(1), Duration::from_hours(24))
}
