use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use bytesize::ByteSize;
use serde::Deserialize;
use shared_rules::hyper::HTTP1_MIN_READ_BUFFER_BYTES;

use crate::listener_limits::{
    DEFAULT_ADMISSION_WAIT, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT,
    DEFAULT_REQUEST_TIMEOUT, connection_age_range, connection_count_range, in_flight_range,
    request_budget_fits_drain, request_timeout_range,
};
use crate::validate::{ValidationError, Violations, duration_range, int_range};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct HttpConfig {
    /// Listen address as an IP `host:port`, or `:port` for IPv4
    /// all-interfaces (`0.0.0.0`). Hostnames are refused; load does not
    /// look them up.
    #[serde(deserialize_with = "crate::de::listen_addr")]
    pub addr: SocketAddr,
    /// Total time the platform allows between SIGTERM and SIGKILL. Every
    /// teardown stage draws from it; `drain_timeout` bounds the service's HTTP
    /// drain or the jobs worker's in-flight work drain.
    ///
    /// This crate checks `drain_timeout <= grace_period`. The composition
    /// root still requires leftover budget after the drain for diagnostics,
    /// background join, dependency close, and telemetry flush; that full
    /// rule is not encoded here.
    #[serde(with = "humantime_serde")]
    pub grace_period: Duration,
    /// Bound for the service's HTTP drain, including readiness propagation,
    /// and for the jobs worker's in-flight work drain, including configured
    /// messaging consumption. The process budgets Tokio shutdown separately
    /// within the SIGTERM-to-exit window (`grace_period`).
    #[serde(with = "humantime_serde")]
    pub drain_timeout: Duration,
    /// How long the listener keeps serving after readiness flips off, so a
    /// load balancer notices `/health/ready` failing before connections stop
    /// being accepted. Production-shaped on purpose; local overlays set `0s`.
    #[serde(with = "humantime_serde")]
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
    #[serde(with = "humantime_serde")]
    pub header_read_timeout: Duration,
    /// Budget through the response head, including admission and request body
    /// reads. Features that stream responses own the lifetime of their bodies
    /// and any resources held after the response head.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// HTTP/1 read-buffer ceiling for one request head; overflow answers
    /// hyper-native 431 before the router, not a Problem. HTTP/2 applies the
    /// same number as uncompressed header-list size.
    pub max_header_bytes: ByteSize,
    /// A declared request body length above this ceiling answers 413.
    /// Streaming bodies are checked as handlers or extractors read them.
    pub max_body_bytes: ByteSize,
    /// Concurrent handler executions. A request over the limit waits up to
    /// `admission_wait` for a slot and is then shed with 503. Zero disables
    /// the limit; [`Self::in_flight_cap`] gives the adapter form.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub max_in_flight: u32,
    /// Longest wait for a handler slot, never longer than `request_timeout`;
    /// [`Self::admission_wait_cap`] gives the adapter form. The wait is
    /// spent out of the request budget: a wait near the whole budget admits
    /// requests with no time left to finish, so overload ends in 504s
    /// instead of 503s. Zero sheds at once.
    #[serde(with = "humantime_serde")]
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
    #[serde(with = "humantime_serde")]
    pub max_connection_age: Duration,
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

#[derive(Debug, thiserror::Error)]
#[error(
    "http.grace_period ({grace:?}) must be >= http.drain_timeout ({drain_timeout:?}) plus the {tail:?} {teardown}"
)]
pub struct GraceBudgetError {
    grace: Duration,
    drain_timeout: Duration,
    tail: Duration,
    teardown: &'static str,
}

impl HttpConfig {
    pub fn check_grace_budget(
        &self,
        tail: Duration,
        teardown: &'static str,
    ) -> Result<(), GraceBudgetError> {
        if self.grace_period < self.drain_timeout.saturating_add(tail) {
            return Err(GraceBudgetError {
                grace: self.grace_period,
                drain_timeout: self.drain_timeout,
                tail,
                teardown,
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn connection_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_connections)
    }

    #[must_use]
    pub fn in_flight_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_in_flight)
    }

    #[must_use]
    pub fn admission_wait_cap(&self) -> Duration {
        self.admission_wait.min(self.request_timeout)
    }

    #[must_use]
    pub fn connection_age_cap(&self) -> Option<Duration> {
        (!self.max_connection_age.is_zero()).then_some(self.max_connection_age)
    }

    #[must_use]
    pub fn effective_drain_budget(&self) -> Duration {
        self.drain_timeout
            .saturating_sub(self.readiness_propagation_delay)
    }

    pub(crate) fn validate(&self, v: &mut Violations) -> HttpTimings {
        let second = Duration::from_secs(1);
        let ten_minutes = Duration::from_mins(10);
        let grace_ok = duration_range("http.grace_period", self.grace_period, second..=ten_minutes);
        let drain_ok = duration_range(
            "http.drain_timeout",
            self.drain_timeout,
            second..=ten_minutes,
        );
        let ranges_ok = grace_ok.is_ok() && drain_ok.is_ok();
        let drain_in_range = drain_ok.is_ok();
        v.check(grace_ok);
        v.check(drain_ok);
        let mut drain_fits_grace = false;
        if ranges_ok {
            if self.drain_timeout > self.grace_period {
                v.check(Err(ValidationError::new(
                    "http.drain_timeout",
                    format!(
                        "must be <= http.grace_period ({}), got {}",
                        humantime::format_duration(self.grace_period),
                        humantime::format_duration(self.drain_timeout)
                    ),
                )));
            } else {
                drain_fits_grace = true;
            }
        }
        let mut propagation_fits_drain = false;
        if drain_in_range && drain_fits_grace {
            if self.readiness_propagation_delay >= self.drain_timeout {
                v.check(Err(ValidationError::new(
                    "http.readiness_propagation_delay",
                    format!(
                        "must be less than http.drain_timeout ({}), got {}",
                        humantime::format_duration(self.drain_timeout),
                        humantime::format_duration(self.readiness_propagation_delay)
                    ),
                )));
            } else {
                propagation_fits_drain = true;
            }
        }
        v.check(duration_range(
            "http.header_read_timeout",
            self.header_read_timeout,
            Duration::from_millis(100)..=Duration::from_mins(15),
        ));
        let request_timeout_ok =
            request_timeout_range("http.request_timeout", self.request_timeout);
        let request_timeout_in_range = request_timeout_ok.is_ok();
        v.check(request_timeout_ok);
        if request_timeout_in_range && propagation_fits_drain {
            v.check(request_budget_fits_drain(
                "http.request_timeout",
                self.request_timeout,
                self.effective_drain_budget(),
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
        v.check(duration_range(
            "http.admission_wait",
            self.admission_wait,
            Duration::ZERO..=Duration::from_mins(10),
        ));
        v.check(connection_count_range(
            "http.max_connections",
            self.max_connections,
        ));
        v.check(connection_age_range(
            "http.max_connection_age",
            self.max_connection_age,
        ));
        HttpTimings {
            drain_budget: propagation_fits_drain.then(|| self.effective_drain_budget()),
            request_timeout: request_timeout_in_range.then_some(self.request_timeout),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct HttpTimings {
    pub(crate) drain_budget: Option<Duration>,
    pub(crate) request_timeout: Option<Duration>,
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
            aged.validate(v);
        })
        .unwrap();
        assert_eq!(aged.connection_age_cap(), Some(Duration::from_mins(30)));
        let too_short = HttpConfig {
            max_connection_age: Duration::from_millis(500),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                too_short.validate(v);
            })
            .unwrap_err()
            .names("http.max_connection_age")
        );
    }

    #[test]
    fn defaults_validate() {
        validate_with(|v| {
            HttpConfig::default().validate(v);
        })
        .unwrap();
    }

    #[test]
    fn request_timeout_must_fit_inside_drain() {
        let cfg = HttpConfig {
            request_timeout: Duration::from_secs(11),
            ..HttpConfig::default()
        };
        let errors = validate_with(|v| {
            cfg.validate(v);
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
            short_budget.validate(v);
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
            immediate.validate(v);
        })
        .unwrap();
        assert_eq!(immediate.admission_wait_cap(), Duration::ZERO);
        let unbounded = HttpConfig {
            admission_wait: Duration::from_mins(11),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                unbounded.validate(v);
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
                cfg.validate(v);
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
            unbounded.validate(v);
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
                cfg.validate(v);
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
            behind_google_cloud.validate(v);
        })
        .unwrap();
        let unbounded = HttpConfig {
            header_read_timeout: Duration::from_mins(16),
            ..HttpConfig::default()
        };
        assert!(
            validate_with(|v| {
                unbounded.validate(v);
            })
            .unwrap_err()
            .names("http.header_read_timeout")
        );
    }
}
