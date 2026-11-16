use serde_json::Value;

/// `base` with every member of `extra` set, `null` members included.
pub(crate) fn merged(mut base: Value, extra: &Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

pub(crate) fn runtime(start_paused: bool) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(start_paused)
        .build()
        .unwrap()
}
