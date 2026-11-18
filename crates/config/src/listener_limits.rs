use std::time::Duration;

use crate::validate::{ValidationError, duration_range, int_range};

pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
pub(super) const DEFAULT_MAX_IN_FLIGHT: u32 = 256;
pub(super) const DEFAULT_ADMISSION_WAIT: Duration = Duration::from_secs(1);
pub(super) const DEFAULT_MAX_CONNECTIONS: u32 = 4096;

pub(super) fn admission_wait_range(key: &str, value: Duration) -> Result<(), ValidationError> {
    duration_range(key, value, Duration::ZERO..=Duration::from_mins(10))
}

pub(super) fn admission_wait_cap(wait: Duration, request_timeout: Duration) -> Duration {
    wait.min(request_timeout)
}

pub(super) fn validate_request_timeout(
    key: &str,
    timeout: Duration,
    drain_budget: Option<Duration>,
    subject: &str,
) -> Result<(), ValidationError> {
    duration_range(
        key,
        timeout,
        Duration::from_millis(100)..=Duration::from_mins(10),
    )?;
    if let Some(drain) = drain_budget
        && timeout > drain
    {
        return Err(ValidationError::new(
            key,
            format!(
                "must be <= the drain budget after readiness propagation ({}) so in-flight {subject} can finish, got {}",
                crate::de::display_duration(drain),
                crate::de::display_duration(timeout)
            ),
        ));
    }
    Ok(())
}

pub(super) fn in_flight_range(key: &str, value: u32) -> Result<(), ValidationError> {
    int_range(key, value, 0..=100_000)
}

pub(super) fn connection_count_range(key: &str, value: u32) -> Result<(), ValidationError> {
    int_range(key, value, 0..=1_000_000)
}

pub(super) fn connection_age_range(key: &str, value: Duration) -> Result<(), ValidationError> {
    if value.is_zero() {
        return Ok(());
    }
    duration_range(
        key,
        value,
        Duration::from_secs(1)..=Duration::from_hours(24),
    )
}
