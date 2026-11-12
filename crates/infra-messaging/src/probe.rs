//! The readiness probe and the publication occupancy it samples.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::FutureExt as _;
use health::{Probe, ProbeError};

use crate::messaging::Shared;
use crate::topology::{classify_topology, validate_server};

/// Timestamped native permit occupancy, sampled only by the existing probe.
///
/// Labelled by connection name so two resources in one process report their
/// own capacity and occupancy rather than overwriting one series.
#[derive(Debug)]
pub(crate) struct PublishWork {
    in_flight: metrics::Gauge,
    last_observed: metrics::Gauge,
}

impl PublishWork {
    pub(crate) fn register(capacity: usize, connection: &str) -> Self {
        let connection = connection.to_owned();
        let in_flight =
            metrics::gauge!("messaging_publish_work_in_flight", "connection" => connection.clone());
        let last_observed = metrics::gauge!(
            "messaging_publish_work_last_observed_timestamp_seconds",
            "connection" => connection.clone()
        );
        in_flight.set(f64::NAN);
        last_observed.set(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "capacity is bounded by the 64 MiB window"
        )]
        metrics::gauge!("messaging_publish_capacity", "connection" => connection)
            .set(capacity as f64);
        Self {
            in_flight,
            last_observed,
        }
    }

    pub(crate) fn observe(&self, context: &async_nats::jetstream::Context) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "occupancy is bounded by publication capacity"
        )]
        self.in_flight.set(context.in_flight_publishes() as f64);
        self.last_observed.set(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(f64::NAN, |elapsed| elapsed.as_secs_f64()),
        );
    }
}

/// A readiness probe whose I/O runs only in health's background refresher.
///
/// Each check passively records native publication permits, including abandoned
/// ACK cleanup, before any health refusal. `messaging_publish_work_in_flight`
/// is NaN and its observation timestamp is zero until the first check. Treat
/// stale, future or invalid timestamps and failed scrapes as unknown occupancy.
/// This is a dated snapshot, not a continuously live count or delivery proof;
/// `messaging_publish_capacity` is the fixed native admission limit.
#[derive(Clone, Debug)]
pub struct MessagingProbe {
    pub(crate) shared: Arc<Shared>,
}

#[async_trait::async_trait]
impl Probe for MessagingProbe {
    fn name(&self) -> &'static str {
        "messaging"
    }

    async fn check(&self) -> Result<(), ProbeError> {
        self.shared.publish_work.observe(&self.shared.jetstream);
        self.check_local_state()?;
        self.shared
            .jetstream
            .get_stream(&self.shared.source_stream)
            .await
            .map_err(|error| {
                let failure = classify_topology(&error);
                tracing::warn!(reason = %failure, "messaging_readiness_probe_failed");
                ProbeError::new("messaging source stream is unavailable")
            })?;
        self.check_local_state()
    }
}

impl MessagingProbe {
    fn check_local_state(&self) -> Result<(), ProbeError> {
        if self.shared.is_draining() {
            return Err(ProbeError::new("messaging is draining"));
        }
        if self.shared.has_failed() {
            return Err(ProbeError::new("messaging consumer failed"));
        }
        if self
            .shared
            .connection
            .client
            .wait_closed()
            .now_or_never()
            .is_some()
            || !matches!(
                self.shared.connection.client.connection_state(),
                async_nats::connection::State::Connected
            )
        {
            return Err(ProbeError::new("messaging connection is unavailable"));
        }
        validate_server(&self.shared.connection.client, self.shared.envelope_limit)
            .map_err(|_| ProbeError::new("messaging server admission is unavailable"))
    }
}

/// Describes the publication occupancy gauges to the installed recorder.
/// Repeating it is harmless.
pub(crate) fn describe_metrics() {
    use metrics::{Unit, describe_gauge};

    describe_gauge!(
        "messaging_publish_work_in_flight",
        "Last observed native publication permits, including abandoned ACK cleanup; NaN before observation"
    );
    describe_gauge!(
        "messaging_publish_work_last_observed_timestamp_seconds",
        Unit::Seconds,
        "Unix time of the native publication observation; zero before observation"
    );
    describe_gauge!(
        "messaging_publish_capacity",
        "Fixed native publication permit capacity"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_occupancy_starts_unknown_with_fixed_capacity() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let _work = PublishWork::register(17, "worker-a");
            let _other = PublishWork::register(256, "worker-b");
        });
        let scrape = recorder.handle().render();
        assert!(scrape.contains("messaging_publish_work_in_flight{connection=\"worker-a\"} NaN"));
        assert!(scrape.contains(
            "messaging_publish_work_last_observed_timestamp_seconds{connection=\"worker-a\"} 0"
        ));
        assert!(scrape.contains("messaging_publish_capacity{connection=\"worker-a\"} 17"));
        assert!(scrape.contains("messaging_publish_capacity{connection=\"worker-b\"} 256"));
    }
}
