use metrics::{Key, Level, Metadata, Recorder as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use metrics_util::{CompositeKey, MetricKind};

const RESTORE_METADATA: Metadata<'static> = Metadata::new(module_path!(), Level::INFO, None);

#[derive(Debug)]
pub(crate) struct Gauges {
    recorder: DebuggingRecorder,
    snapshotter: Snapshotter,
}

impl Default for Gauges {
    fn default() -> Self {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        Self {
            recorder,
            snapshotter,
        }
    }
}

impl Gauges {
    pub(crate) fn recorder(&self) -> &DebuggingRecorder {
        &self.recorder
    }

    pub(crate) fn get(&self, name: &'static str, kind: Option<&'static str>) -> f64 {
        let target = match kind {
            Some(kind) => Key::from_parts(name, &[("kind", kind)]),
            None => Key::from_name(name),
        };
        // `Snapshotter::snapshot` swaps every gauge to zero; restore each raw
        // value so subsequent decrements and untouched series stay intact.
        let series = self.snapshotter.snapshot().into_hashmap();
        for (composite, (_, _, value)) in &series {
            if let DebugValue::Gauge(gauge) = value {
                self.recorder
                    .register_gauge(composite.key(), &RESTORE_METADATA)
                    .set(gauge.into_inner());
            }
        }
        let found = match series.get(&CompositeKey::new(MetricKind::Gauge, target.clone())) {
            Some((_, _, DebugValue::Gauge(gauge))) => Some(gauge.into_inner()),
            _ => None,
        };
        found.unwrap_or_else(|| panic!("missing gauge {target}"))
    }
}
