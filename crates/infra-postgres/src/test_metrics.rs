use std::collections::HashMap;

use metrics::{Key, SharedString, Unit};
use metrics_util::debugging::DebugValue;
use metrics_util::{CompositeKey, MetricKind};

pub(crate) type Series = HashMap<CompositeKey, (Option<Unit>, Option<SharedString>, DebugValue)>;

fn value<'a>(series: &'a Series, kind: MetricKind, key: &Key) -> Option<&'a DebugValue> {
    series
        .get(&CompositeKey::new(kind, key.clone()))
        .map(|(_, _, value)| value)
}

pub(crate) fn counter(series: &Series, key: &Key) -> Option<u64> {
    match value(series, MetricKind::Counter, key)? {
        DebugValue::Counter(count) => Some(*count),
        _ => None,
    }
}

pub(crate) fn gauge(series: &Series, key: &Key) -> Option<f64> {
    match value(series, MetricKind::Gauge, key)? {
        DebugValue::Gauge(gauge) => Some(gauge.into_inner()),
        _ => None,
    }
}

pub(crate) fn histogram_len(series: &Series, key: &Key) -> Option<usize> {
    match value(series, MetricKind::Histogram, key)? {
        DebugValue::Histogram(samples) => Some(samples.len()),
        _ => None,
    }
}
