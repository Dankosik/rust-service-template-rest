//! The bounded trace carrier stored on a job and linked from its attempt span.

use std::str::FromStr;

use opentelemetry::{
    global,
    propagation::{Extractor, Injector},
    trace::{TraceContextExt, TraceState},
};
use tracing_opentelemetry::OpenTelemetrySpanExt;

const TRACEPARENT: &str = "traceparent";
const TRACESTATE: &str = "tracestate";
const TRACEPARENT_MAX_BYTES: usize = 256;
const TRACESTATE_MAX_BYTES: usize = 512;

/// Capture the current context with the installed text-map propagator.
pub(crate) fn capture() -> (Option<String>, Option<String>) {
    let mut carrier = Carrier::default();
    let context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut carrier);
    });
    carrier.into_stored()
}

/// Link `span` to a valid, remote stored context without changing its parent.
pub(crate) fn link(span: &tracing::Span, trace_parent: Option<&str>, trace_state: Option<&str>) {
    let Some(carrier) = Carrier::from_stored(trace_parent, trace_state) else {
        return;
    };
    let context = global::get_text_map_propagator(|propagator| {
        propagator.extract_with_context(&opentelemetry::Context::new(), &carrier)
    });
    let span_ref = TraceContextExt::span(&context);
    let context = span_ref.span_context();
    if context.is_valid() && context.is_remote() {
        span.add_link(context.clone());
    }
}

#[derive(Default)]
struct Carrier {
    trace_parent: Option<String>,
    trace_state: Option<String>,
}

impl Carrier {
    fn from_stored(trace_parent: Option<&str>, trace_state: Option<&str>) -> Option<Self> {
        if trace_parent.is_some_and(|value| !is_allowed(value, TRACEPARENT_MAX_BYTES)) {
            return None;
        }
        let trace_parent = trace_parent.map(str::to_owned);
        let trace_state = match trace_state {
            None | Some("") => None,
            Some(value)
                if is_allowed(value, TRACESTATE_MAX_BYTES)
                    && TraceState::from_str(value).is_ok() =>
            {
                Some(value.to_owned())
            }
            Some(_) => return None,
        };
        Some(Self {
            trace_parent,
            trace_state,
        })
    }

    fn into_stored(self) -> (Option<String>, Option<String>) {
        match Self::from_stored(self.trace_parent.as_deref(), self.trace_state.as_deref()) {
            Some(carrier) => (carrier.trace_parent, carrier.trace_state),
            None => (None, None),
        }
    }
}

impl Injector for Carrier {
    fn set(&mut self, key: &str, value: String) {
        match key {
            TRACEPARENT => self.trace_parent = Some(value),
            TRACESTATE => self.trace_state = Some(value),
            _ => {}
        }
    }
}

impl Extractor for Carrier {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            TRACEPARENT => self.trace_parent.as_deref(),
            TRACESTATE => self.trace_state.as_deref(),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        [
            self.trace_parent.as_ref().map(|_| TRACEPARENT),
            self.trace_state.as_ref().map(|_| TRACESTATE),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

fn is_allowed(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
        && value.is_ascii()
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use opentelemetry::propagation::{Extractor, Injector};

    use super::{Carrier, TRACEPARENT, TRACEPARENT_MAX_BYTES, TRACESTATE, TRACESTATE_MAX_BYTES};

    const PARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn accepts_legacy_parent_only_context() {
        let carrier = Carrier::from_stored(Some(PARENT), None).expect("legacy parent is allowed");

        assert_eq!(carrier.get(TRACEPARENT), Some(PARENT));
        assert_eq!(carrier.get(TRACESTATE), None);
        assert_eq!(carrier.keys(), vec![TRACEPARENT]);
    }

    #[test]
    fn empty_tracestate_is_absent() {
        let carrier = Carrier::from_stored(Some(PARENT), Some("")).expect("empty state is absent");

        assert_eq!(carrier.get(TRACESTATE), None);
    }

    #[test]
    fn rejects_malformed_tracestate_before_extraction() {
        assert!(Carrier::from_stored(Some(PARENT), Some("not-a-list-member")).is_none());
    }

    #[test]
    fn rejects_non_ascii_control_and_overbound_carriers() {
        assert!(Carrier::from_stored(Some("parent\n"), None).is_none());
        assert!(Carrier::from_stored(Some("parenté"), None).is_none());
        assert!(
            Carrier::from_stored(Some(&"a".repeat(TRACEPARENT_MAX_BYTES + 1)), None,).is_none()
        );
        assert!(
            Carrier::from_stored(
                Some(PARENT),
                Some(&format!("vendor={}", "a".repeat(TRACESTATE_MAX_BYTES - 6))),
            )
            .is_none()
        );
    }

    #[test]
    fn ignores_non_trace_injection_keys() {
        let mut carrier = Carrier::default();

        carrier.set("baggage", "account=123".to_owned());
        carrier.set(TRACEPARENT, PARENT.to_owned());

        assert_eq!(carrier.keys(), vec![TRACEPARENT]);
        assert_eq!(carrier.get("baggage"), None);
    }
}
