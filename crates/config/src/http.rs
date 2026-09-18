//! HTTP listener, request budgets, capacity bounds, and the drain budget.

use std::net::SocketAddr;
use std::time::Duration;

use bytesize::ByteSize;
use serde::Deserialize;

use crate::validate::{ValidationError, duration_range, int_range, non_empty, socket_addr};

/// hyper refuses a read buffer below this size; the header-bytes bound is
/// implemented through that buffer.
pub const MIN_HEADER_BYTES: u64 = 8 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct HttpConfig {
    /// Listen address as `host:port`, or `:port` for every interface.
    pub addr: String,
    /// Total time the platform allows between SIGTERM and SIGKILL. Every
    /// teardown stage draws from it; `shutdown_timeout` bounds only the HTTP
    /// drain.
    ///
    /// This crate checks `shutdown_timeout <= grace_period`. The composition
    /// root still requires leftover budget after the drain for diagnostics,
    /// background join, dependency close, and telemetry flush; that full
    /// rule is not encoded here.
    #[serde(with = "humantime_serde")]
    pub grace_period: Duration,
    /// Bound for the HTTP drain, including the readiness propagation delay
    /// in front of it.
    #[serde(with = "humantime_serde")]
    pub shutdown_timeout: Duration,
    /// How long the listener keeps serving after readiness flips off, so a
    /// load balancer notices `/health/ready` failing before connections stop
    /// being accepted. Production-shaped on purpose; local overlays set `0s`.
    #[serde(with = "humantime_serde")]
    pub readiness_propagation_delay: Duration,
    /// Time a connection may take to deliver a complete request head. hyper
    /// restarts this timer whenever an HTTP/1 connection goes idle, so it is
    /// also the keep-alive idle bound.
    #[serde(with = "humantime_serde")]
    pub header_read_timeout: Duration,
    /// Per-request handler budget. It is the only bound on how long one
    /// request may hold a task and its pooled resources; body reads happen
    /// inside it because extractors run inside the handler future.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Read-buffer ceiling for one request head; overflow answers 431.
    pub max_header_bytes: ByteSize,
    /// Request body ceiling; overflow answers 413.
    pub max_body_bytes: ByteSize,
    /// Concurrent handler executions before shedding with 503. Zero disables
    /// shedding. The composition root maps zero to `None` on the adapter
    /// policy type.
    pub max_in_flight: u32,
    /// Accepted connections at once. At the cap the accept loop closes the
    /// socket with no HTTP response. Zero accepts without a bound. The
    /// composition root maps zero to `None` on the adapter policy type.
    pub max_connections: u32,
    /// Re-enable access logging for `/health/live` and `/health/ready`.
    pub access_log_health_probes: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            addr: ":8080".to_owned(),
            grace_period: Duration::from_secs(45),
            // 25s rather than the whole grace period: the teardown after the
            // drain (diagnostics, background join, dependency close,
            // telemetry flush) needs the remaining budget. Load does not
            // encode that tail; the composition root does.
            shutdown_timeout: Duration::from_secs(25),
            readiness_propagation_delay: Duration::from_secs(15),
            header_read_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(8),
            max_header_bytes: ByteSize::kib(16),
            max_body_bytes: ByteSize::mib(1),
            max_in_flight: 256,
            // Well above max_in_flight: shedding answers 503 with Retry-After;
            // the connection cap closes the socket with no HTTP response. The
            // headroom keeps the informative rejection common.
            max_connections: 4096,
            access_log_health_probes: false,
        }
    }
}

impl HttpConfig {
    /// The parsed listen address.
    ///
    /// # Errors
    ///
    /// Returns the validation error for a malformed `http.addr`.
    pub fn listen_addr(&self) -> Result<SocketAddr, ValidationError> {
        socket_addr("http.addr", &self.addr)
    }

    /// Drain budget left after the readiness propagation delay.
    #[must_use]
    pub fn effective_drain_budget(&self) -> Duration {
        self.shutdown_timeout
            .saturating_sub(self.readiness_propagation_delay)
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        non_empty("http.addr", &self.addr)?;
        self.listen_addr()?;
        let second = Duration::from_secs(1);
        let ten_minutes = Duration::from_secs(600);
        let hundred_ms = Duration::from_millis(100);
        duration_range("http.grace_period", self.grace_period, second, ten_minutes)?;
        duration_range(
            "http.shutdown_timeout",
            self.shutdown_timeout,
            second,
            ten_minutes,
        )?;
        if self.shutdown_timeout > self.grace_period {
            return Err(ValidationError::new(
                "http.shutdown_timeout",
                format!(
                    "must be <= http.grace_period ({})",
                    humantime::format_duration(self.grace_period)
                ),
            ));
        }
        if self.readiness_propagation_delay >= self.shutdown_timeout {
            return Err(ValidationError::new(
                "http.readiness_propagation_delay",
                "must be less than http.shutdown_timeout",
            ));
        }
        duration_range(
            "http.header_read_timeout",
            self.header_read_timeout,
            hundred_ms,
            Duration::from_secs(300),
        )?;
        duration_range(
            "http.request_timeout",
            self.request_timeout,
            hundred_ms,
            ten_minutes,
        )?;

        let drain = self.effective_drain_budget();
        if self.request_timeout > drain {
            return Err(ValidationError::new(
                "http.request_timeout",
                format!(
                    "must be <= the drain budget after readiness propagation ({}) so in-flight requests can finish",
                    humantime::format_duration(drain)
                ),
            ));
        }

        int_range(
            "http.max_header_bytes",
            self.max_header_bytes.as_u64(),
            MIN_HEADER_BYTES,
            ByteSize::mib(1).as_u64(),
        )?;
        int_range(
            "http.max_body_bytes",
            self.max_body_bytes.as_u64(),
            1,
            ByteSize::gib(1).as_u64(),
        )?;
        int_range(
            "http.max_in_flight",
            u64::from(self.max_in_flight),
            0,
            100_000,
        )?;
        int_range(
            "http.max_connections",
            u64::from(self.max_connections),
            0,
            1_000_000,
        )?;
        if self.max_connections != 0 && self.max_connections < self.max_in_flight {
            return Err(ValidationError::new(
                "http.max_connections",
                format!("must be >= http.max_in_flight ({})", self.max_in_flight),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        HttpConfig::default().validate().unwrap();
    }

    #[test]
    fn request_timeout_must_fit_inside_drain() {
        let cfg = HttpConfig {
            request_timeout: Duration::from_secs(11),
            ..HttpConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.key, "http.request_timeout");
    }

    #[test]
    fn propagation_delay_must_leave_drain_budget() {
        let cfg = HttpConfig {
            readiness_propagation_delay: Duration::from_secs(25),
            ..HttpConfig::default()
        };
        assert_eq!(
            cfg.validate().unwrap_err().key,
            "http.readiness_propagation_delay"
        );
    }

    #[test]
    fn connection_cap_cannot_undercut_in_flight() {
        let cfg = HttpConfig {
            max_connections: 100,
            ..HttpConfig::default()
        };
        assert_eq!(cfg.validate().unwrap_err().key, "http.max_connections");
        let unbounded = HttpConfig {
            max_connections: 0,
            ..HttpConfig::default()
        };
        unbounded.validate().unwrap();
    }

    #[test]
    fn header_bytes_below_hyper_minimum_rejected() {
        let cfg = HttpConfig {
            max_header_bytes: ByteSize::kib(4),
            ..HttpConfig::default()
        };
        assert_eq!(cfg.validate().unwrap_err().key, "http.max_header_bytes");
    }
}
