use std::time::Duration;

use enum_map::{Enum, EnumMap};

#[derive(derive_more::Debug)]
#[debug("Outcomes")]
pub(crate) struct Outcomes<O: Enum> {
    handles: EnumMap<O, (metrics::Counter, metrics::Histogram)>,
}

impl<O: Enum> Outcomes<O> {
    pub(crate) fn register(
        total: &'static str,
        duration: &'static str,
        label_key: &'static str,
        event_type: Option<&'static str>,
        label: fn(O) -> &'static str,
    ) -> Self {
        let handles = EnumMap::from_fn(|outcome: O| {
            let mut labels = vec![metrics::Label::from_static_parts(label_key, label(outcome))];
            if let Some(event_type) = event_type {
                labels.push(metrics::Label::from_static_parts("event_type", event_type));
            }
            (
                metrics::counter!(total, labels.clone()),
                metrics::histogram!(duration, labels),
            )
        });
        Self { handles }
    }

    pub(crate) fn record(&self, outcome: O, elapsed: Duration) {
        let (total, duration) = &self.handles[outcome];
        total.increment(1);
        duration.record(elapsed);
    }
}

pub(crate) fn record_span_outcome(
    span: &tracing::Span,
    outcome: &'static str,
    error_type: Option<&'static str>,
) {
    span.record("outcome", outcome);
    if let Some(error_type) = error_type {
        span.record("error.type", error_type);
        span.record("otel.status_code", "ERROR");
    }
}
