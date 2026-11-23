use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::ops::RangeInclusive;
use std::time::Duration;

use bytesize::ByteSize;
use serde::Deserialize;
use shared_rules::hyper::HTTP1_MIN_READ_BUFFER_BYTES;

use crate::de::{Keep, Mode, ModeMarker, StrictDuration, When};
use crate::listener_limits::{
    DEFAULT_ADMISSION_WAIT, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT,
    DEFAULT_REQUEST_TIMEOUT, ListenerKind, RawListenerLimits,
};
use crate::validate::{Rejected, ValidationError, Violations, duration_range, nonzero_usize_range};

const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024 * 1024;
const GRACE_AND_DRAIN_RANGE: RangeInclusive<Duration> =
    Duration::from_secs(1)..=Duration::from_mins(10);

pub(crate) type HttpInput = HttpFields;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownTail {
    pub duration: Duration,
    pub description: &'static str,
}

impl ShutdownTail {
    #[must_use]
    pub const fn new(duration: Duration, description: &'static str) -> Self {
        Self {
            duration,
            description,
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default(bound = false))]
#[serde(bound(deserialize = ""), deny_unknown_fields, default)]
pub(crate) struct HttpFields<
    Propagation: Mode = Keep,
    const DEFAULT_GRACE_SECS: u64 = 45,
    const DEFAULT_DRAIN_SECS: u64 = 25,
> {
    /// Listen address as an IP `host:port`, or `:port` for IPv4
    /// all-interfaces (`0.0.0.0`). Hostnames are refused.
    #[serde_as(as = "crate::de::ListenAddr")]
    #[educe(Default(expression = SocketAddr::from(([0, 0, 0, 0], 8080))))]
    addr: SocketAddr,
    /// Total time the platform allows between SIGTERM and SIGKILL.
    /// `drain_timeout` plus the process's [`ShutdownTail`] must fit inside it.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(DEFAULT_GRACE_SECS)))]
    grace_period: Duration,
    /// Bound for the HTTP drain. In the service this includes
    /// `readiness_propagation_delay`. The worker budgets its drain separately.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(DEFAULT_DRAIN_SECS)))]
    drain_timeout: Duration,
    /// Time the service keeps serving after readiness flips off so a load
    /// balancer observes `/health/ready` failing before the listener stops
    /// accepting connections. The worker ignores this field.
    #[serde_as(as = "When<Propagation, StrictDuration>")]
    #[educe(Default(expression = Duration::from_secs(15)))]
    readiness_propagation_delay: Duration,
    /// Time limit for reading a complete HTTP/1.1 request head. It also bounds
    /// keep-alive idle time. Set it above the upstream load balancer's idle
    /// timeout to avoid 502s when it reuses a connection the listener closed.
    /// The default clears the common 60s timeout. Google Cloud load balancers
    /// hold backend connections for 600s.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(75)))]
    header_read_timeout: Duration,
    /// Budget through the response head, including admission and request body
    /// reads. Features that stream responses own the lifetime of their bodies
    /// and any resources held after the response head.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = DEFAULT_REQUEST_TIMEOUT))]
    request_timeout: Duration,
    /// Read-buffer ceiling for one request head. Overflow answers with a
    /// Hyper 431 before the router.
    #[educe(Default(expression = ByteSize::kib(16)))]
    max_header_bytes: ByteSize,
    /// A declared request body length above this ceiling answers 413.
    /// Streaming bodies are checked as handlers or extractors read them.
    #[educe(Default(expression = ByteSize::mib(1)))]
    max_body_bytes: ByteSize,
    /// Requests in handlers at once, each counted until its response head.
    /// Requests above the limit wait up to `admission_wait` before receiving
    /// 503. `http_server_active_requests` includes waiting requests.
    /// Zero disables the limit.
    #[serde_as(as = "crate::de::StrictCount")]
    #[educe(Default(expression = DEFAULT_MAX_IN_FLIGHT))]
    max_in_flight: u32,
    /// Longest wait for a handler slot. It consumes part of the request budget
    /// and cannot outlast `request_timeout`. Zero sheds at once.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = DEFAULT_ADMISSION_WAIT))]
    admission_wait: Duration,
    /// Accepted connections at once. At the cap the accept loop closes the
    /// socket with no HTTP response. Zero accepts without a bound.
    /// A connection carries one request at a time, so `max_in_flight` takes
    /// effect only below this cap.
    #[serde_as(as = "crate::de::StrictCount")]
    #[educe(Default(expression = DEFAULT_MAX_CONNECTIONS))]
    max_connections: u32,
    /// Age after which a connection is told to finish and close, so its client
    /// reconnects and is balanced again. Each connection's age is spread by
    /// up to 10% either way. Zero leaves connection age unbounded.
    #[serde_as(as = "StrictDuration")]
    max_connection_age: Duration,
    #[serde_as(as = "crate::de::StrictBool")]
    access_log_health_probes: bool,
    #[serde_as(as = "crate::de::StrictBool")]
    trust_trace_context: bool,
    #[serde(skip)]
    _mode: ModeMarker<Propagation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpListenerConfig {
    pub addr: SocketAddr,
    pub header_read_timeout: Duration,
    pub request_timeout: Duration,
    pub max_header_bytes: NonZeroUsize,
    pub max_body_bytes: NonZeroUsize,
    pub max_in_flight: Option<NonZeroU32>,
    pub admission_wait: Duration,
    pub max_connections: Option<NonZeroU32>,
    pub max_connection_age: Option<Duration>,
    pub access_log_health_probes: bool,
    pub trust_trace_context: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpConfig {
    listener: HttpListenerConfig,
    pub grace_period: Duration,
    pub drain_timeout: Duration,
    pub readiness_propagation_delay: Duration,
}

impl HttpConfig {
    #[must_use]
    pub const fn listener(&self) -> &HttpListenerConfig {
        &self.listener
    }

    #[must_use]
    pub const fn effective_drain_budget(&self) -> Duration {
        drain_budget(self.drain_timeout, self.readiness_propagation_delay)
    }
}

#[cfg(test)]
pub(crate) const NO_SHUTDOWN_TAIL: ShutdownTail = ShutdownTail::new(Duration::ZERO, "");

const fn drain_budget(drain_timeout: Duration, readiness_propagation_delay: Duration) -> Duration {
    drain_timeout.saturating_sub(readiness_propagation_delay)
}

fn validate_grace_and_drain(
    grace_period: Duration,
    drain_timeout: Duration,
    tail: ShutdownTail,
    v: &mut Violations,
) -> Result<(), Rejected> {
    let grace = v.record(duration_range(
        "http.grace_period",
        grace_period,
        GRACE_AND_DRAIN_RANGE,
    ));
    let drain = v.record(duration_range(
        "http.drain_timeout",
        drain_timeout,
        GRACE_AND_DRAIN_RANGE,
    ));
    grace.and(drain)?;
    if drain_timeout > grace_period {
        return Err(v.reject(ValidationError::new(
            "http.drain_timeout",
            format!(
                "must be <= http.grace_period ({}), got {}",
                crate::de::display_duration(grace_period),
                crate::de::display_duration(drain_timeout)
            ),
        )));
    }
    if grace_period < drain_timeout.saturating_add(tail.duration) {
        v.reject(ValidationError::new(
            "http.grace_period",
            format!(
                "({}) must be >= http.drain_timeout ({}) plus the {} {}",
                crate::de::display_duration(grace_period),
                crate::de::display_duration(drain_timeout),
                crate::de::display_duration(tail.duration),
                tail.description
            ),
        ));
    }
    Ok(())
}

impl<Propagation: Mode, const DEFAULT_GRACE_SECS: u64, const DEFAULT_DRAIN_SECS: u64>
    HttpFields<Propagation, DEFAULT_GRACE_SECS, DEFAULT_DRAIN_SECS>
{
    // template:begin temporal:http-valid-drain-timeout
    pub(crate) fn valid_drain_timeout(&self) -> Option<Duration> {
        GRACE_AND_DRAIN_RANGE
            .contains(&self.drain_timeout)
            .then_some(self.drain_timeout)
    }
    // template:end temporal:http-valid-drain-timeout

    fn admit_listener(
        &self,
        drain_budget: Option<Duration>,
        v: &mut Violations,
    ) -> Result<HttpListenerConfig, Rejected> {
        v.check(duration_range(
            "http.header_read_timeout",
            self.header_read_timeout,
            Duration::from_millis(100)..=Duration::from_mins(15),
        ));
        let max_header_bytes = v.record(nonzero_usize_range(
            "http.max_header_bytes",
            self.max_header_bytes.as_u64(),
            usize::from(HTTP1_MIN_READ_BUFFER_BYTES)..=MAX_HEADER_BYTES,
        ));
        let max_body_bytes = v.record(nonzero_usize_range(
            "http.max_body_bytes",
            self.max_body_bytes.as_u64(),
            1..=MAX_BODY_BYTES,
        ));
        let limits = RawListenerLimits {
            request_timeout: self.request_timeout,
            max_in_flight: self.max_in_flight,
            admission_wait: self.admission_wait,
            max_connections: self.max_connections,
            max_connection_age: self.max_connection_age,
        }
        .admit(ListenerKind::HTTP, drain_budget, v);
        Ok(HttpListenerConfig {
            addr: self.addr,
            header_read_timeout: self.header_read_timeout,
            request_timeout: limits.request_timeout,
            max_header_bytes: max_header_bytes?,
            max_body_bytes: max_body_bytes?,
            max_in_flight: limits.max_in_flight,
            admission_wait: limits.admission_wait,
            max_connections: limits.max_connections,
            max_connection_age: limits.max_connection_age,
            access_log_health_probes: self.access_log_health_probes,
            trust_trace_context: self.trust_trace_context,
        })
    }
}

impl HttpInput {
    pub(crate) fn admit(
        self,
        tail: ShutdownTail,
        v: &mut Violations,
    ) -> (Result<HttpConfig, Rejected>, Option<Duration>) {
        let drain_budget = validate_grace_and_drain(self.grace_period, self.drain_timeout, tail, v)
            .and_then(|()| self.admit_propagation_delay(v))
            .ok();
        let config = self
            .admit_listener(drain_budget, v)
            .map(|listener| HttpConfig {
                listener,
                grace_period: self.grace_period,
                drain_timeout: self.drain_timeout,
                readiness_propagation_delay: self.readiness_propagation_delay,
            });
        (config, drain_budget)
    }

    fn admit_propagation_delay(&self, v: &mut Violations) -> Result<Duration, Rejected> {
        if self.readiness_propagation_delay >= self.drain_timeout {
            return Err(v.reject(ValidationError::new(
                "http.readiness_propagation_delay",
                format!(
                    "must be less than http.drain_timeout ({}), got {}",
                    crate::de::display_duration(self.drain_timeout),
                    crate::de::display_duration(self.readiness_propagation_delay)
                ),
            )));
        }
        Ok(drain_budget(
            self.drain_timeout,
            self.readiness_propagation_delay,
        ))
    }
}

// template:begin worker:worker-http-input
pub(crate) type WorkerHttpInput = HttpFields<crate::de::Skip>;
// template:end worker:worker-http-input

// template:begin worker-http:worker-http-config
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerHttpConfig {
    listener: HttpListenerConfig,
    pub grace_period: Duration,
    pub drain_timeout: Duration,
}

impl WorkerHttpConfig {
    #[must_use]
    pub const fn listener(&self) -> &HttpListenerConfig {
        &self.listener
    }
}

impl<const DEFAULT_GRACE_SECS: u64, const DEFAULT_DRAIN_SECS: u64>
    HttpFields<crate::de::Skip, DEFAULT_GRACE_SECS, DEFAULT_DRAIN_SECS>
{
    pub(crate) fn admit(
        self,
        tail: ShutdownTail,
        v: &mut Violations,
    ) -> Result<WorkerHttpConfig, Rejected> {
        let _ = validate_grace_and_drain(self.grace_period, self.drain_timeout, tail, v);
        let listener = self.admit_listener(None, v)?;
        Ok(WorkerHttpConfig {
            listener,
            grace_period: self.grace_period,
            drain_timeout: self.drain_timeout,
        })
    }
}
// template:end worker-http:worker-http-config

// template:begin temporal:temporal-worker-http-input
pub(crate) type TemporalWorkerHttpInput = HttpFields<crate::de::Skip, 90, 60>;
// template:end temporal:temporal-worker-http-input

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ValidationErrors;
    use crate::validate::admit_with;

    fn admit(input: HttpInput) -> Result<HttpConfig, ValidationErrors> {
        admit_with(|v| input.admit(super::NO_SHUTDOWN_TAIL, v).0)
    }

    #[test]
    fn a_connection_age_is_off_at_zero_or_at_least_a_second() {
        let defaults = admit(HttpInput::default()).unwrap();
        assert_eq!(defaults.listener().max_connection_age, None);
        let aged = admit(HttpInput {
            max_connection_age: Duration::from_mins(30),
            ..HttpInput::default()
        })
        .unwrap();
        assert_eq!(
            aged.listener().max_connection_age,
            Some(Duration::from_mins(30))
        );
        let too_short = HttpInput {
            max_connection_age: Duration::from_millis(500),
            ..HttpInput::default()
        };
        assert!(
            admit(too_short)
                .unwrap_err()
                .names("http.max_connection_age")
        );
    }

    #[test]
    fn grace_budget_validates_inside_admission_when_tail_is_supplied() {
        let tail = ShutdownTail::new(Duration::from_secs(18), "teardown tail");
        let tight = HttpInput {
            grace_period: Duration::from_secs(42),
            drain_timeout: Duration::from_secs(25),
            ..HttpInput::default()
        };
        let errors = admit_with(|v| tight.admit(tail, v).0).unwrap_err();
        assert!(errors.names("http.grace_period"), "{errors}");
        assert!(
            errors
                .to_string()
                .contains("(42s) must be >= http.drain_timeout (25s) plus the 18s teardown tail"),
            "{errors}"
        );
    }

    #[test]
    fn request_timeout_must_fit_inside_drain() {
        let input = HttpInput {
            request_timeout: Duration::from_secs(11),
            ..HttpInput::default()
        };
        let errors = admit(input).unwrap_err();
        assert!(errors.names("http.request_timeout"));
    }

    #[test]
    fn the_admission_wait_defaults_to_a_second_and_never_outlasts_the_request_budget() {
        let defaults = admit(HttpInput::default()).unwrap();
        assert_eq!(defaults.listener().admission_wait, Duration::from_secs(1));
        let short_budget = admit(HttpInput {
            request_timeout: Duration::from_millis(500),
            ..HttpInput::default()
        })
        .unwrap();
        assert_eq!(
            short_budget.listener().admission_wait,
            Duration::from_millis(500)
        );
        let immediate = admit(HttpInput {
            admission_wait: Duration::ZERO,
            ..HttpInput::default()
        })
        .unwrap();
        assert_eq!(immediate.listener().admission_wait, Duration::ZERO);
        let unbounded = HttpInput {
            admission_wait: Duration::from_mins(11),
            ..HttpInput::default()
        };
        assert!(admit(unbounded).unwrap_err().names("http.admission_wait"));
    }

    #[test]
    fn propagation_delay_must_leave_drain_budget() {
        let input = HttpInput {
            readiness_propagation_delay: Duration::from_secs(25),
            ..HttpInput::default()
        };
        assert!(
            admit(input)
                .unwrap_err()
                .names("http.readiness_propagation_delay")
        );
    }

    #[test]
    fn connection_cap_defaults_to_4096_and_zero_disables_it() {
        let unbounded = admit(HttpInput {
            max_connections: 0,
            ..HttpInput::default()
        })
        .unwrap();
        assert_eq!(unbounded.listener().max_connections, None);
        assert_eq!(
            admit(HttpInput::default())
                .unwrap()
                .listener()
                .max_connections
                .map(NonZeroU32::get),
            Some(4096)
        );
    }

    #[test]
    fn header_bytes_below_hyper_minimum_rejected() {
        let input = HttpInput {
            max_header_bytes: ByteSize::kib(4),
            ..HttpInput::default()
        };
        assert!(admit(input).unwrap_err().names("http.max_header_bytes"));
    }

    #[test]
    fn an_idle_connection_outlives_common_balancer_idle_timeouts() {
        let defaults = admit(HttpInput::default()).unwrap();
        assert!(defaults.listener().header_read_timeout > Duration::from_mins(1));
        let behind_google_cloud = HttpInput {
            header_read_timeout: Duration::from_secs(620),
            ..HttpInput::default()
        };
        admit(behind_google_cloud).unwrap();
        let unbounded = HttpInput {
            header_read_timeout: Duration::from_mins(16),
            ..HttpInput::default()
        };
        assert!(
            admit(unbounded)
                .unwrap_err()
                .names("http.header_read_timeout")
        );
    }

    // template:begin worker:worker-http-config-tests
    #[test]
    fn worker_http_drain_timeout_is_decoupled_from_readiness_propagation_and_request_timeout() {
        let loaded = crate::test_support::load_worker_env(&[
            ("APP__HTTP__DRAIN_TIMEOUT", "1s"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "not a duration"),
        ]);
        assert_eq!(loaded.http().drain_timeout, Duration::from_secs(1));
        assert_eq!(
            loaded.http().listener().request_timeout,
            Duration::from_secs(8)
        );
    }

    #[test]
    fn worker_http_ignores_shared_propagation_delay_in_toml_and_rejects_unknown_keys() {
        let ignored = "drain_timeout = \"10s\"\nreadiness_propagation_delay = { bad = 1 }\n";
        let input: WorkerHttpInput = toml::from_str(ignored).unwrap();
        let worker = admit_with(|v| input.admit(super::NO_SHUTDOWN_TAIL, v)).unwrap();
        assert_eq!(worker.drain_timeout, Duration::from_secs(10));
        assert!(toml::from_str::<HttpInput>(ignored).is_err());
        assert!(toml::from_str::<WorkerHttpInput>("bogus = 1\n").is_err());
    }
    // template:end worker:worker-http-config-tests

    // template:begin temporal:temporal-worker-http-config-test-head
    #[test]
    fn temporal_worker_http_defaults_to_90s_and_60s_without_changing_service_or_worker_defaults() {
        let service = admit(HttpInput::default()).unwrap();
        assert_eq!(service.grace_period, Duration::from_secs(45));
        assert_eq!(service.drain_timeout, Duration::from_secs(25));
        // template:end temporal:temporal-worker-http-config-test-head

        // template:begin worker-temporal:temporal-worker-http-config-test-worker
        let jobs_worker =
            admit_with(|v| WorkerHttpInput::default().admit(super::NO_SHUTDOWN_TAIL, v)).unwrap();
        assert_eq!(jobs_worker.grace_period, Duration::from_secs(45));
        assert_eq!(jobs_worker.drain_timeout, Duration::from_secs(25));
        // template:end worker-temporal:temporal-worker-http-config-test-worker

        // template:begin temporal:temporal-worker-http-config-test-tail
        let temporal_input: TemporalWorkerHttpInput =
            toml::from_str("readiness_propagation_delay = { bad = 1 }\n").unwrap();
        let temporal = admit_with(|v| temporal_input.admit(super::NO_SHUTDOWN_TAIL, v)).unwrap();
        assert_eq!(temporal.grace_period, Duration::from_secs(90));
        assert_eq!(temporal.drain_timeout, Duration::from_secs(60));
        assert!(toml::from_str::<TemporalWorkerHttpInput>("bogus = 1\n").is_err());
    }
    // template:end temporal:temporal-worker-http-config-test-tail
}
