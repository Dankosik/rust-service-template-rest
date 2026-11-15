//! A metrics recorder that keeps gauge values, for tests that read them back.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use metrics::atomics::AtomicU64;
use metrics::{Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit};

#[derive(Default)]
pub(crate) struct Gauges(Mutex<HashMap<Key, Arc<AtomicU64>>>);

impl Gauges {
    pub(crate) fn get(&self, name: &'static str, kind: Option<&'static str>) -> f64 {
        let key = match kind {
            Some(kind) => Key::from_parts(name, &[("kind", kind)]),
            None => Key::from_name(name),
        };
        f64::from_bits(self.0.lock().unwrap()[&key].load(Ordering::Acquire))
    }
}

impl Recorder for Gauges {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(Arc::clone(
            self.0.lock().unwrap().entry(key.clone()).or_default(),
        ))
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}
