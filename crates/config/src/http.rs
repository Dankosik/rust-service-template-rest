use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use bytesize::ByteSize;
use serde::Deserialize;
use shared_rules::hyper::HTTP1_MIN_READ_BUFFER_BYTES;

use crate::listener_limits::{
    DEFAULT_ADMISSION_WAIT, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT,
    DEFAULT_REQUEST_TIMEOUT, admission_wait_cap, admission_wait_range, connection_age_range,
    connection_count_range, in_flight_range, request_budget_fits_drain, request_timeout_range,
};
use crate::validate::{ValidationError, Violations, duration_range, int_range};

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct HttpConfig {
    /// Listen address as an IP `host:port`, or `:port` for IPv4
    /// all-interfaces (`0.0.0.0`). Hostnames are refused; load does not
    /// look them up.
    #[serde(deserialize_with = "crate::de::listen_addr")]
    pub addr: SocketAddr,
    /// Total time the platform allows between SIGTERM and SIGKILL. Every
    /// teardown stage draws from it: `drain_timeout` plus the composition
    /// root's [`ShutdownTail`] must fit inside `grace_period`.
    #[serde(deserialize_with = "crate::de::duration")]
    pub grace_period: Duration,
    /// Bound for the service's HTTP drain, including
    /// `readiness_propagation_delay`. The process budgets post-drain teardown
    /// and Tokio shutdown within `grace_period`.
    #[serde(deserialize_with = "crate::de::duration")]
    pub drain_timeout: Duration,
    /// How long the listener keeps serving after readiness flips off, so a
    /// load balancer notices `/health/ready` failing before connections stop
    /// being accepted. Production-shaped on purpose; local overlays set `0s`.
    #[serde(deserialize_with = "crate::de::duration")]
    pub readiness_propagation_delay: Duration,
    /// Separate time limits of this duration apply to protocol detection and
    /// reading a complete HTTP/1 request head. On the first request, those
    /// waits can add up to twice this value. The HTTP/1 timer also bounds
    /// keep-alive idle time, so keep it above the idle timeout of whatever
    /// reuses connections to this listener: a balancer that reuses a
    /// connection the service has just closed answers 502. The default
    /// clears the common 60 s (AWS ALB, nginx upstreams, Railway); Google
    /// Cloud load balancers hold backend connections for 600 s. HTTP/2
    /// request headers have a size limit (`max_header_bytes`) and no read
    /// timeout from this setting. HTTP/2 idle uses a separate PING cadence.
    #[serde(deserialize_with = "crate::de::duration")]
    pub header_read_timeout: Duration,
    /// Budget through the response head, including admission and request body
    /// reads. Features that stream responses own the lifetime of their bodies
    /// and any resources held after the response head.
    #[serde(deserialize_with = "crate::de::duration")]
    pub request_timeout: Duration,
    /// HTTP/1 read-buffer ceiling for one request head; overflow answers
    /// hyper-native 431 before the router, not a Problem. HTTP/2 applies the
    /// same number as uncompressed header-list size.
    pub max_header_bytes: ByteSize,
    /// A declared request body length above this ceiling answers 413.
    /// Streaming bodies are checked as handlers or extractors read them.
    pub max_body_bytes: ByteSize,
    /// Requests in handlers at once, each counted until its response head:
    /// a body still streaming holds no slot. A request over the limit waits
    /// up to `admission_wait` for a slot and is then shed with 503; while it
    /// waits, `http_server_active_requests` already counts it. Zero disables
    /// the limit; [`Self::in_flight_cap`] gives the adapter form.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub max_in_flight: u32,
    /// Longest wait for a handler slot, never longer than `request_timeout`;
    /// [`Self::admission_wait_cap`] gives the adapter form. The wait is
    /// spent out of the request budget: a wait near the whole budget admits
    /// requests with no time left to finish, so overload ends in 504s
    /// instead of 503s. Zero sheds at once.
    #[serde(deserialize_with = "crate::de::duration")]
    pub admission_wait: Duration,
    /// Accepted connections at once. At the cap the accept loop closes the
    /// socket with no HTTP response. Zero accepts without a bound;
    /// [`Self::connection_cap`] gives the adapter form.
    /// HTTP/2 can run several handlers on one connection, so the two limits
    /// can be sized independently.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub max_connections: u32,
    /// Age after which a connection is told to finish and close, so its
    /// client reconnects and is balanced again. Each connection's age is
    /// spread by up to 10% either way. Zero keeps a connection for as long
    /// as its peer does.
    #[serde(deserialize_with = "crate::de::duration")]
    pub max_connection_age: Duration,
    #[serde(deserialize_with = "crate::de::boolean")]
    pub access_log_health_probes: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            grace_period: Duration::from_secs(45),
            drain_timeout: Duration::from_secs(25),
            readiness_propagation_delay: Duration::from_secs(15),
            header_read_timeout: Duration::from_secs(75),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_header_bytes: ByteSize::kib(16),
            max_body_bytes: ByteSize::mib(1),
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            admission_wait: DEFAULT_ADMISSION_WAIT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_connection_age: Duration::ZERO,
            access_log_health_probes: false,
        }
    }
}

fn check_grace_budget_pair(
    grace_period: Duration,
    drain_timeout: Duration,
    tail: ShutdownTail,
) -> Result<(), ValidationError> {
    if grace_period < drain_timeout.saturating_add(tail.duration) {
        return Err(ValidationError::new(
            "http.grace_period",
            format!(
                "({grace_period:?}) must be >= http.drain_timeout ({drain_timeout:?}) plus the {:?} {}",
                tail.duration, tail.description
            ),
        ));
    }
    Ok(())
}

struct DrainCheck {
    in_range: bool,
    fits_grace: bool,
}

fn validate_grace_and_drain(
    grace_period: Duration,
    drain_timeout: Duration,
    tail: Option<ShutdownTail>,
    v: &mut Violations,
) -> DrainCheck {
    let range = Duration::from_secs(1)..=Duration::from_mins(10);
    let grace_ok = duration_range("http.grace_period", grace_period, range.clone());
    let drain_ok = duration_range("http.drain_timeout", drain_timeout, range);
    let ranges_ok = grace_ok.is_ok() && drain_ok.is_ok();
    let in_range = drain_ok.is_ok();
    v.check(grace_ok);
    v.check(drain_ok);
    let mut fits_grace = false;
    if ranges_ok {
        if drain_timeout > grace_period {
            v.check(Err(ValidationError::new(
                "http.drain_timeout",
                format!(
                    "must be <= http.grace_period ({}), got {}",
                    crate::de::display_duration(grace_period),
                    crate::de::display_duration(drain_timeout)
                ),
            )));
        } else {
            fits_grace = true;
            if let Some(tail) = tail {
                v.check(check_grace_budget_pair(grace_period, drain_timeout, tail));
            }
        }
    }
    DrainCheck {
        in_range,
        fits_grace,
    }
}

struct ListenerLimits {
    header_read_timeout: Duration,
    request_timeout: Duration,
    max_header_bytes: ByteSize,
    max_body_bytes: ByteSize,
    max_in_flight: u32,
    admission_wait: Duration,
    max_connections: u32,
    max_connection_age: Duration,
}

impl ListenerLimits {
    fn validate(&self, drain_budget: Option<Duration>, v: &mut Violations) -> Option<Duration> {
        v.check(duration_range(
            "http.header_read_timeout",
            self.header_read_timeout,
            Duration::from_millis(100)..=Duration::from_mins(15),
        ));
        let request_timeout_ok =
            request_timeout_range("http.request_timeout", self.request_timeout);
        let request_timeout_in_range = request_timeout_ok.is_ok();
        v.check(request_timeout_ok);
        if let Some(drain_budget) = drain_budget
            && request_timeout_in_range
        {
            v.check(request_budget_fits_drain(
                "http.request_timeout",
                self.request_timeout,
                drain_budget,
                "requests",
            ));
        }
        v.check(int_range(
            "http.max_header_bytes",
            self.max_header_bytes.as_u64(),
            u64::from(HTTP1_MIN_READ_BUFFER_BYTES)..=ByteSize::mib(1).as_u64(),
        ));
        v.check(int_range(
            "http.max_body_bytes",
            self.max_body_bytes.as_u64(),
            1..=ByteSize::gib(1).as_u64(),
        ));
        v.check(in_flight_range("http.max_in_flight", self.max_in_flight));
        v.check(admission_wait_range(
            "http.admission_wait",
            self.admission_wait,
        ));
        v.check(connection_count_range(
            "http.max_connections",
            self.max_connections,
        ));
        v.check(connection_age_range(
            "http.max_connection_age",
            self.max_connection_age,
        ));
        request_timeout_in_range.then_some(self.request_timeout)
    }
}

impl HttpConfig {
    pub fn check_grace_budget(&self, tail: ShutdownTail) -> Result<(), ValidationError> {
        check_grace_budget_pair(self.grace_period, self.drain_timeout, tail)
    }

    #[must_use]
    pub fn connection_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_connections)
    }

    #[must_use]
    pub fn effective_drain_budget(&self) -> Duration {
        self.drain_timeout
            .saturating_sub(self.readiness_propagation_delay)
    }

    #[must_use]
    pub fn in_flight_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_in_flight)
    }

    #[must_use]
    pub fn admission_wait_cap(&self) -> Duration {
        admission_wait_cap(self.admission_wait, self.request_timeout)
    }

    #[must_use]
    pub fn connection_age_cap(&self) -> Option<Duration> {
        (!self.max_connection_age.is_zero()).then_some(self.max_connection_age)
    }

    pub(crate) fn validate(&self, tail: Option<ShutdownTail>, v: &mut Violations) -> HttpTimings {
        let drain = validate_grace_and_drain(self.grace_period, self.drain_timeout, tail, v);
        let mut propagation_fits_drain = false;
        if drain.in_range && drain.fits_grace {
            if self.readiness_propagation_delay >= self.drain_timeout {
                v.check(Err(ValidationError::new(
                    "http.readiness_propagation_delay",
                    format!(
                        "must be less than http.drain_timeout ({}), got {}",
                        crate::de::display_duration(self.drain_timeout),
                        crate::de::display_duration(self.readiness_propagation_delay)
                    ),
                )));
            } else {
                propagation_fits_drain = true;
            }
        }
        let drain_budget = propagation_fits_drain.then(|| self.effective_drain_budget());
        #[allow(
            unused_variables,
            reason = "only the cache profile consumes the validated HTTP request timeout"
        )]
        let request_timeout = ListenerLimits {
            header_read_timeout: self.header_read_timeout,
            request_timeout: self.request_timeout,
            max_header_bytes: self.max_header_bytes,
            max_body_bytes: self.max_body_bytes,
            max_in_flight: self.max_in_flight,
            admission_wait: self.admission_wait,
            max_connections: self.max_connections,
            max_connection_age: self.max_connection_age,
        }
        .validate(drain_budget, v);
        HttpTimings {
            // template:begin grpc:http-timings-drain-value
            drain_budget,
            // template:end grpc:http-timings-drain-value
            // template:begin cache:http-timings-request-value
            request_timeout,
            // template:end cache:http-timings-request-value
        }
    }
}

// template:begin worker:worker-http-config
#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct WorkerHttpInput {
    #[serde(deserialize_with = "crate::de::listen_addr")]
    addr: SocketAddr,
    #[serde(deserialize_with = "crate::de::boolean")]
    access_log_health_probes: bool,
    max_header_bytes: ByteSize,
    max_body_bytes: ByteSize,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_in_flight: u32,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_connections: u32,
    #[serde(deserialize_with = "crate::de::duration")]
    header_read_timeout: Duration,
    #[serde(deserialize_with = "crate::de::duration")]
    request_timeout: Duration,
    #[serde(deserialize_with = "crate::de::duration")]
    admission_wait: Duration,
    #[serde(deserialize_with = "crate::de::duration")]
    max_connection_age: Duration,
    #[serde(deserialize_with = "crate::de::duration")]
    grace_period: Duration,
    #[serde(deserialize_with = "crate::de::duration")]
    drain_timeout: Duration,
    readiness_propagation_delay: serde::de::IgnoredAny,
}

impl Default for WorkerHttpInput {
    fn default() -> Self {
        let defaults = WorkerHttpConfig::default();
        Self {
            addr: defaults.addr,
            access_log_health_probes: defaults.access_log_health_probes,
            max_header_bytes: defaults.max_header_bytes,
            max_body_bytes: defaults.max_body_bytes,
            max_in_flight: defaults.max_in_flight,
            max_connections: defaults.max_connections,
            header_read_timeout: defaults.header_read_timeout,
            request_timeout: defaults.request_timeout,
            admission_wait: defaults.admission_wait,
            max_connection_age: defaults.max_connection_age,
            grace_period: defaults.grace_period,
            drain_timeout: defaults.drain_timeout,
            readiness_propagation_delay: serde::de::IgnoredAny,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "WorkerHttpInput")]
pub struct WorkerHttpConfig {
    pub addr: SocketAddr,
    pub grace_period: Duration,
    pub drain_timeout: Duration,
    pub header_read_timeout: Duration,
    pub request_timeout: Duration,
    pub max_header_bytes: ByteSize,
    pub max_body_bytes: ByteSize,
    pub max_in_flight: u32,
    pub admission_wait: Duration,
    pub max_connections: u32,
    pub max_connection_age: Duration,
    pub access_log_health_probes: bool,
}

impl From<WorkerHttpInput> for WorkerHttpConfig {
    fn from(input: WorkerHttpInput) -> Self {
        Self {
            addr: input.addr,
            grace_period: input.grace_period,
            drain_timeout: input.drain_timeout,
            header_read_timeout: input.header_read_timeout,
            request_timeout: input.request_timeout,
            max_header_bytes: input.max_header_bytes,
            max_body_bytes: input.max_body_bytes,
            max_in_flight: input.max_in_flight,
            admission_wait: input.admission_wait,
            max_connections: input.max_connections,
            max_connection_age: input.max_connection_age,
            access_log_health_probes: input.access_log_health_probes,
        }
    }
}

impl Default for WorkerHttpConfig {
    fn default() -> Self {
        let HttpConfig {
            addr,
            grace_period,
            drain_timeout,
            readiness_propagation_delay: _,
            header_read_timeout,
            request_timeout,
            max_header_bytes,
            max_body_bytes,
            max_in_flight,
            admission_wait,
            max_connections,
            max_connection_age,
            access_log_health_probes,
        } = HttpConfig::default();
        Self {
            addr,
            grace_period,
            drain_timeout,
            header_read_timeout,
            request_timeout,
            max_header_bytes,
            max_body_bytes,
            max_in_flight,
            admission_wait,
            max_connections,
            max_connection_age,
            access_log_health_probes,
        }
    }
}

impl WorkerHttpConfig {
    pub(crate) fn validate(&self, tail: Option<ShutdownTail>, v: &mut Violations) {
        validate_grace_and_drain(self.grace_period, self.drain_timeout, tail, v);
        ListenerLimits {
            header_read_timeout: self.header_read_timeout,
            request_timeout: self.request_timeout,
            max_header_bytes: self.max_header_bytes,
            max_body_bytes: self.max_body_bytes,
            max_in_flight: self.max_in_flight,
            admission_wait: self.admission_wait,
            max_connections: self.max_connections,
            max_connection_age: self.max_connection_age,
        }
        .validate(None, v);
    }

    #[must_use]
    pub fn admission_wait_cap(&self) -> Duration {
        admission_wait_cap(self.admission_wait, self.request_timeout)
    }

    #[must_use]
    pub fn connection_age_cap(&self) -> Option<Duration> {
        (!self.max_connection_age.is_zero()).then_some(self.max_connection_age)
    }

    pub fn check_grace_budget(&self, tail: ShutdownTail) -> Result<(), ValidationError> {
        check_grace_budget_pair(self.grace_period, self.drain_timeout, tail)
    }

    #[must_use]
    pub fn in_flight_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_in_flight)
    }

    #[must_use]
    pub fn connection_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_connections)
    }
}
// template:end worker:worker-http-config

#[derive(Clone, Copy)]
pub(crate) struct HttpTimings {
    // template:begin grpc:http-timings-drain-field
    pub(crate) drain_budget: Option<Duration>,
    // template:end grpc:http-timings-drain-field
    // template:begin cache:http-timings-request-field
    pub(crate) request_timeout: Option<Duration>,
    // template:end cache:http-timings-request-field
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate_with;

    #[test]
    fn a_connection_age_is_off_at_zero_or_at_least_a_second() {
        assert_eq!(HttpConfig::default().connection_age_cap(), None);
        let aged = HttpConfig {
            max_connection_age: Duration::from_mins(30),
            ..HttpConfig::default()
        };
        validate_with(|v| {
            aged.validate(None, v);
        })
        .unwrap();
        assert_eq!(aged.connection_age_cap(), Some(Duration::from_mins(30)));
        let too_short = HttpConfig {
            max_connection_age: Duration::from_millis(500),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                too_short.validate(None, v);
            })
            .unwrap_err()
            .names("http.max_connection_age")
        );
    }

    #[test]
    fn defaults_validate() {
        validate_with(|v| {
            HttpConfig::default().validate(None, v);
        })
        .unwrap();
    }

    #[test]
    fn grace_budget_validates_inside_admission_when_tail_is_supplied() {
        let tail = ShutdownTail::new(Duration::from_secs(18), "teardown tail");
        let tight = HttpConfig {
            grace_period: Duration::from_secs(42),
            drain_timeout: Duration::from_secs(25),
            ..HttpConfig::default()
        };
        let errors = validate_with(|v| {
            tight.validate(Some(tail), v);
        })
        .unwrap_err();
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
        let cfg = HttpConfig {
            request_timeout: Duration::from_secs(11),
            ..HttpConfig::default()
        };
        let errors = validate_with(|v| {
            cfg.validate(None, v);
        })
        .unwrap_err();
        assert!(errors.names("http.request_timeout"));
    }

    #[test]
    fn the_admission_wait_defaults_to_a_second_and_never_outlasts_the_request_budget() {
        let defaults = HttpConfig::default();
        assert_eq!(defaults.admission_wait_cap(), Duration::from_secs(1));
        let short_budget = HttpConfig {
            request_timeout: Duration::from_millis(500),
            ..HttpConfig::default()
        };
        validate_with(|v| {
            short_budget.validate(None, v);
        })
        .unwrap();
        assert_eq!(
            short_budget.admission_wait_cap(),
            Duration::from_millis(500)
        );
        let immediate = HttpConfig {
            admission_wait: Duration::ZERO,
            ..HttpConfig::default()
        };
        validate_with(|v| {
            immediate.validate(None, v);
        })
        .unwrap();
        assert_eq!(immediate.admission_wait_cap(), Duration::ZERO);
        let unbounded = HttpConfig {
            admission_wait: Duration::from_mins(11),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                unbounded.validate(None, v);
            })
            .unwrap_err()
            .names("http.admission_wait")
        );
    }

    #[test]
    fn propagation_delay_must_leave_drain_budget() {
        let cfg = HttpConfig {
            readiness_propagation_delay: Duration::from_secs(25),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                cfg.validate(None, v);
            })
            .unwrap_err()
            .names("http.readiness_propagation_delay")
        );
    }

    #[test]
    fn connection_cap_defaults_to_4096_and_zero_disables_it() {
        let unbounded = HttpConfig {
            max_connections: 0,
            ..HttpConfig::default()
        };
        validate_with(|v| {
            unbounded.validate(None, v);
        })
        .unwrap();
        assert_eq!(unbounded.connection_cap(), None);
        assert_eq!(
            HttpConfig::default().connection_cap().map(NonZeroU32::get),
            Some(4096)
        );
    }

    #[test]
    fn header_bytes_below_hyper_minimum_rejected() {
        let cfg = HttpConfig {
            max_header_bytes: ByteSize::kib(4),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                cfg.validate(None, v);
            })
            .unwrap_err()
            .names("http.max_header_bytes")
        );
    }

    #[test]
    fn an_idle_connection_outlives_common_balancer_idle_timeouts() {
        assert!(HttpConfig::default().header_read_timeout > Duration::from_mins(1));
        let behind_google_cloud = HttpConfig {
            header_read_timeout: Duration::from_secs(620),
            ..HttpConfig::default()
        };
        validate_with(|v| {
            behind_google_cloud.validate(None, v);
        })
        .unwrap();
        let unbounded = HttpConfig {
            header_read_timeout: Duration::from_mins(16),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                unbounded.validate(None, v);
            })
            .unwrap_err()
            .names("http.header_read_timeout")
        );
    }

    // template:begin worker:worker-http-config-tests
    #[test]
    fn worker_http_drain_timeout_is_decoupled_from_readiness_propagation_and_request_timeout() {
        for drain_timeout in [Duration::from_secs(10), Duration::from_secs(1)] {
            let worker = WorkerHttpConfig {
                drain_timeout,
                ..WorkerHttpConfig::default()
            };
            assert_eq!(worker.request_timeout, Duration::from_secs(8));
            validate_with(|v| worker.validate(None, v)).unwrap();
        }
        let loaded = crate::test_support::load_worker_env(&[
            ("APP__HTTP__DRAIN_TIMEOUT", "1s"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
        ]);
        assert_eq!(loaded.http.drain_timeout, Duration::from_secs(1));
        assert_eq!(loaded.http.request_timeout, Duration::from_secs(8));
    }

    #[test]
    fn worker_http_ignores_shared_propagation_delay_in_toml_and_rejects_unknown_keys() {
        let decoded: WorkerHttpConfig =
            toml::from_str("drain_timeout = \"10s\"\nreadiness_propagation_delay = \"0s\"\n")
                .unwrap();
        assert_eq!(decoded.drain_timeout, Duration::from_secs(10));
        assert!(toml::from_str::<WorkerHttpConfig>("bogus = 1\n").is_err());
    }
    // template:end worker:worker-http-config-tests
}
