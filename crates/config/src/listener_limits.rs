use std::num::NonZeroU32;
use std::time::Duration;

use crate::validate::{ValidationError, Violations, duration_range, int_range};

pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
pub(super) const DEFAULT_MAX_IN_FLIGHT: u32 = 256;
pub(super) const DEFAULT_ADMISSION_WAIT: Duration = Duration::from_secs(1);
pub(super) const DEFAULT_MAX_CONNECTIONS: u32 = 4096;

pub(super) struct RawListenerLimits {
    pub(super) request_timeout: Duration,
    pub(super) max_in_flight: u32,
    pub(super) admission_wait: Duration,
    pub(super) max_connections: u32,
    pub(super) max_connection_age: Duration,
}

pub(super) struct AdmittedListenerLimits {
    pub(super) request_timeout: Duration,
    pub(super) max_in_flight: Option<NonZeroU32>,
    pub(super) admission_wait: Duration,
    pub(super) max_connections: Option<NonZeroU32>,
    pub(super) max_connection_age: Option<Duration>,
}

impl RawListenerLimits {
    pub(super) fn admit(
        self,
        section: &str,
        drain_budget: Option<Duration>,
        subject: &str,
        violations: &mut Violations,
    ) -> AdmittedListenerLimits {
        violations.check(validate_request_timeout(
            &format!("{section}.request_timeout"),
            self.request_timeout,
            drain_budget,
            subject,
        ));
        violations.check(int_range(
            &format!("{section}.max_in_flight"),
            self.max_in_flight,
            0..=100_000,
        ));
        violations.check(duration_range(
            &format!("{section}.admission_wait"),
            self.admission_wait,
            Duration::ZERO..=Duration::from_mins(10),
        ));
        violations.check(int_range(
            &format!("{section}.max_connections"),
            self.max_connections,
            0..=1_000_000,
        ));
        violations.check(connection_age_range(
            &format!("{section}.max_connection_age"),
            self.max_connection_age,
        ));
        AdmittedListenerLimits {
            request_timeout: self.request_timeout,
            max_in_flight: NonZeroU32::new(self.max_in_flight),
            admission_wait: self.admission_wait.min(self.request_timeout),
            max_connections: NonZeroU32::new(self.max_connections),
            max_connection_age: (!self.max_connection_age.is_zero())
                .then_some(self.max_connection_age),
        }
    }
}

fn validate_request_timeout(
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

fn connection_age_range(key: &str, value: Duration) -> Result<(), ValidationError> {
    if value.is_zero() {
        return Ok(());
    }
    duration_range(
        key,
        value,
        Duration::from_secs(1)..=Duration::from_hours(24),
    )
}
