use metrics::{Key, Level, Metadata, Recorder as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};

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
        let mut found = None;
        // `Snapshotter::snapshot` swaps every gauge to zero; restore each raw
        // value so subsequent decrements and untouched series stay intact.
        for (composite, _, _, value) in self.snapshotter.snapshot().into_vec() {
            if let DebugValue::Gauge(gauge) = value {
                let raw = gauge.into_inner();
                self.recorder
                    .register_gauge(composite.key(), &RESTORE_METADATA)
                    .set(raw);
                if composite.key() == &target {
                    found = Some(raw);
                }
            }
        }
        found.unwrap_or_else(|| panic!("missing gauge {target}"))
    }
}
