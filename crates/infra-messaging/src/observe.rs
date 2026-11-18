use std::time::Duration;

use enum_map::{Enum, EnumMap};

pub(crate) trait OutcomeLabel: Enum {
    fn label(self) -> &'static str;
}

macro_rules! outcome_label {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($variant:ident => $label:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, ::enum_map::Enum)]
        $vis enum $name {
            $($variant),+
        }

        impl $crate::observe::OutcomeLabel for $name {
            fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }
        }
    };
}
pub(crate) use outcome_label;

#[derive(derive_more::Debug)]
#[debug("Outcomes")]
pub(crate) struct Outcomes<O: OutcomeLabel> {
    handles: EnumMap<O, (metrics::Counter, metrics::Histogram)>,
}

impl<O: OutcomeLabel> Outcomes<O> {
    pub(crate) fn register(
        total: &'static str,
        duration: &'static str,
        label_key: &'static str,
        event_type: Option<&'static str>,
    ) -> Self {
        let handles = EnumMap::from_fn(|outcome: O| {
            let mut labels = vec![metrics::Label::from_static_parts(
                label_key,
                outcome.label(),
            )];
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
        duration.record(elapsed.as_secs_f64());
    }
}

pub(crate) fn record_span_outcome(span: &tracing::Span, label: &'static str, failed: bool) {
    span.record("outcome", label);
    if failed {
        span.record("error.type", label);
        span.record("otel.status_code", "ERROR");
    }
}
