use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::FutureExt as _;
use health::{Probe, ProbeError};
use parking_lot::Mutex;

use crate::messaging::Shared;
use crate::topology::{classify_topology, validate_server};

const PUBLISH_WORK_IN_FLIGHT: &str = "messaging_publish_work_in_flight";
const PUBLISH_WORK_LAST_OBSERVED_SECONDS: &str =
    "messaging_publish_work_last_observed_timestamp_seconds";
const PUBLISH_CAPACITY: &str = "messaging_publish_capacity";

static PUBLISH_SLOTS: Mutex<BTreeMap<String, BTreeSet<usize>>> = Mutex::new(BTreeMap::new());

#[derive(Debug)]
pub(crate) struct PublishWorkMetrics {
    connection: String,
    slot: usize,
    capacity: metrics::Gauge,
    in_flight: metrics::Gauge,
    last_observed: metrics::Gauge,
}

impl PublishWorkMetrics {
    pub(crate) fn register(capacity: usize, connection: &str) -> Self {
        let slot = {
            let mut active = PUBLISH_SLOTS.lock();
            let slots = active.entry(connection.to_owned()).or_default();
            let mut slot = 0;
            while !slots.insert(slot) {
                slot += 1;
            }
            slot
        };
        let connection = connection.to_owned();
        let mut labels = vec![metrics::Label::new("connection", connection.clone())];
        if slot > 0 {
            labels.push(metrics::Label::new("connection_slot", slot.to_string()));
        }
        let in_flight = metrics::gauge!(PUBLISH_WORK_IN_FLIGHT, labels.iter());
        let last_observed = metrics::gauge!(PUBLISH_WORK_LAST_OBSERVED_SECONDS, labels.iter());
        let capacity_gauge = metrics::gauge!(PUBLISH_CAPACITY, labels);
        in_flight.set(f64::NAN);
        last_observed.set(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "capacity is bounded by the 64 MiB window"
        )]
        capacity_gauge.set(capacity as f64);
        Self {
            connection,
            slot,
            capacity: capacity_gauge,
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

impl Drop for PublishWorkMetrics {
    fn drop(&mut self) {
        self.in_flight.set(f64::NAN);
        self.last_observed.set(0.0);
        self.capacity.set(f64::NAN);
        let mut active = PUBLISH_SLOTS.lock();
        if let Entry::Occupied(mut entry) = active.entry(std::mem::take(&mut self.connection)) {
            let slots = entry.get_mut();
            slots.remove(&self.slot);
            if slots.is_empty() {
                entry.remove();
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct MessagingProbe {
    pub(crate) shared: Arc<Shared>,
}

#[health::async_trait]
impl Probe for MessagingProbe {
    fn name(&self) -> &'static str {
        "messaging"
    }

    async fn check(&self) -> Result<(), ProbeError> {
        self.shared
            .publish_work_metrics
            .observe(&self.shared.jetstream);
        self.check_local_state()?;
        match self
            .shared
            .jetstream
            .get_stream(&self.shared.source_stream)
            .await
        {
            Ok(_) => self.check_local_state(),
            Err(error) => {
                let failure = classify_topology(&error);
                tracing::warn!(reason = %failure, "messaging_readiness_probe_failed");
                Err(ProbeError::from_static(
                    "messaging source stream is unavailable",
                ))
            }
        }
    }
}

impl MessagingProbe {
    fn check_local_state(&self) -> Result<(), ProbeError> {
        if self.shared.is_draining() {
            return Err(ProbeError::from_static("messaging is draining"));
        }
        if self.shared.has_failed() {
            return Err(ProbeError::from_static("messaging consumer failed"));
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
            return Err(ProbeError::from_static(
                "messaging connection is unavailable",
            ));
        }
        validate_server(
            &self.shared.connection.client,
            self.shared.envelope_limit_bytes,
        )
        .map_err(|refusal| {
            ProbeError::format(format_args!(
                "messaging server admission is unavailable ({})",
                refusal.error_type()
            ))
        })
    }
}

pub(crate) fn describe_metrics() {
    use metrics::{Unit, describe_gauge};

    describe_gauge!(
        PUBLISH_WORK_IN_FLIGHT,
        "Last observed native publication permits, including abandoned ACK cleanup; NaN before observation"
    );
    describe_gauge!(
        PUBLISH_WORK_LAST_OBSERVED_SECONDS,
        Unit::Seconds,
        "Unix time of the native publication observation; zero before observation"
    );
    describe_gauge!(PUBLISH_CAPACITY, "Fixed native publication permit capacity");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_publishers_keep_separate_capacity_and_reuse_retired_series() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let _work = PublishWorkMetrics::register(17, "worker-a");
            let other = PublishWorkMetrics::register(256, "worker-a");
            let handle = recorder.handle();
            let scrape = handle.render();
            assert!(
                scrape.contains("messaging_publish_work_in_flight{connection=\"worker-a\"} NaN")
            );
            assert!(scrape.contains(
                "messaging_publish_work_last_observed_timestamp_seconds{connection=\"worker-a\"} 0"
            ));
            assert!(scrape.contains("messaging_publish_capacity{connection=\"worker-a\"} 17"));
            let other_capacity = |value| {
                format!(
                    "messaging_publish_capacity{{connection=\"worker-a\",connection_slot=\"1\"}} {value}"
                )
            };
            assert!(scrape.contains(&other_capacity("256")));
            drop(other);
            assert!(handle.render().contains(&other_capacity("NaN")));
            let _replacement = PublishWorkMetrics::register(32, "worker-a");
            assert!(handle.render().contains(&other_capacity("32")));
        });
    }
}
