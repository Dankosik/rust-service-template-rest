//! Closed outcome vocabularies and their pre-registered metric handles.

use std::marker::PhantomData;
use std::time::Duration;

/// A closed outcome vocabulary whose metric label each variant owns.
pub(crate) trait OutcomeLabel: Copy + 'static {
    /// Every variant, each at the position [`Self::index`] returns.
    const ALL: &'static [Self];
    fn label(self) -> &'static str;
    fn index(self) -> usize;
}

/// Declares a closed outcome vocabulary together with its labels and metric
/// positions, so `ALL` and `index` cannot diverge from the enum.
macro_rules! outcome_label {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($variant:ident => $label:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        $vis enum $name {
            $($variant),+
        }

        impl $crate::observe::OutcomeLabel for $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            fn index(self) -> usize {
                self as usize
            }
        }
    };
}
pub(crate) use outcome_label;

/// A count and a duration per outcome label, registered once with the
/// installed recorder. Emitting through the macros instead looks each metric
/// up and allocates its labels on every publish and delivery.
pub(crate) struct Outcomes<O> {
    handles: Box<[(metrics::Counter, metrics::Histogram)]>,
    outcome: PhantomData<O>,
}

impl<O: OutcomeLabel> Outcomes<O> {
    /// `event_type`, when given, labels every outcome with that event type.
    pub(crate) fn register(
        total: &'static str,
        duration: &'static str,
        label: &'static str,
        event_type: Option<&'static str>,
    ) -> Self {
        let handles = O::ALL
            .iter()
            .map(|outcome| {
                let mut labels = vec![metrics::Label::from_static_parts(label, outcome.label())];
                if let Some(event_type) = event_type {
                    labels.push(metrics::Label::from_static_parts("event_type", event_type));
                }
                (
                    metrics::counter!(total, labels.clone()),
                    metrics::histogram!(duration, labels),
                )
            })
            .collect();
        Self {
            handles,
            outcome: PhantomData,
        }
    }

    pub(crate) fn record(&self, outcome: O, elapsed: Duration) {
        let (total, duration) = &self.handles[outcome.index()];
        total.increment(1);
        duration.record(elapsed.as_secs_f64());
    }
}

impl<O> std::fmt::Debug for Outcomes<O> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Outcomes")
    }
}

/// [`Outcomes`] finds a variant's handles at its `index`.
#[cfg(test)]
pub(crate) fn assert_all_in_index_order<O: OutcomeLabel + std::fmt::Debug>() {
    for (position, outcome) in O::ALL.iter().enumerate() {
        assert_eq!(outcome.index(), position, "{outcome:?}");
    }
}
