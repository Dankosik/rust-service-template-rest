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
/// W3C Trace Context drops list members longer than this first when
/// `tracestate` must be truncated.
const TRACESTATE_LONG_MEMBER_BYTES: usize = 128;

/// Capture the current context with the installed text-map propagator.
pub(crate) fn capture() -> Carrier {
    let mut carrier = Carrier::default();
    let context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut carrier);
    });
    carrier.validated()
}

/// Link `span` to a valid, remote stored context without changing its parent.
pub(crate) fn link(span: &tracing::Span, carrier: &Carrier) {
    let context = global::get_text_map_propagator(|propagator| {
        propagator.extract_with_context(&opentelemetry::Context::new(), carrier)
    });
    let span_ref = TraceContextExt::span(&context);
    let context = span_ref.span_context();
    if context.is_valid() && context.is_remote() {
        span.add_link(context.clone());
    }
}

/// A W3C trace context as stored on a job.
///
/// Each present value is bounded, ASCII, and control-free; a nonempty
/// `tracestate` also parses as a W3C list. A refused `traceparent` discards the
/// pair; a refused `tracestate` discards only itself, because W3C Trace
/// Context forbids a `tracestate` failure from affecting `traceparent`. A
/// captured `tracestate` over its bound first loses whole members, as that
/// specification orders. So a stored carrier holds only allowed values. This type does not
/// judge `traceparent` syntax: [`link`] adds a link only when extraction
/// reports the context valid and remote. A state-only carrier is storable, and
/// never links for want of a valid `traceparent`.
#[derive(Debug, Default)]
pub(crate) struct Carrier {
    pub(crate) trace_parent: Option<String>,
    pub(crate) trace_state: Option<String>,
}

impl Carrier {
    /// The stored pair without a refused `tracestate`, or `None` when
    /// `traceparent` is refused.
    pub(crate) fn from_stored(
        trace_parent: Option<&str>,
        trace_state: Option<&str>,
    ) -> Option<Self> {
        if trace_parent.is_some_and(|value| !is_allowed(value, TRACEPARENT_MAX_BYTES)) {
            return None;
        }
        let trace_state = trace_state.filter(|value| {
            !value.is_empty()
                && is_allowed(value, TRACESTATE_MAX_BYTES)
                && TraceState::from_str(value).is_ok()
        });
        Some(Self {
            trace_parent: trace_parent.map(str::to_owned),
            trace_state: trace_state.map(str::to_owned),
        })
    }

    /// The injector's pair with every present field allowed and bounded.
    ///
    /// A refused `traceparent` is stored as two absent fields.
    fn validated(self) -> Self {
        let trace_state = self.trace_state.as_deref().map(truncated_state);
        Self::from_stored(self.trace_parent.as_deref(), trace_state.as_deref()).unwrap_or_default()
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

/// `tracestate` within [`TRACESTATE_MAX_BYTES`]: members longer than
/// [`TRACESTATE_LONG_MEMBER_BYTES`] go first, then members from the end.
fn truncated_state(value: &str) -> String {
    if value.len() <= TRACESTATE_MAX_BYTES {
        return value.to_owned();
    }
    let mut members: Vec<&str> = value
        .split(',')
        .map(|member| member.trim_matches([' ', '\t']))
        .filter(|member| !member.is_empty())
        .collect();
    let joined_len = |members: &[&str]| -> usize {
        members.iter().map(|member| member.len()).sum::<usize>() + members.len().saturating_sub(1)
    };
    if joined_len(&members) > TRACESTATE_MAX_BYTES {
        members.retain(|member| member.len() <= TRACESTATE_LONG_MEMBER_BYTES);
    }
    while joined_len(&members) > TRACESTATE_MAX_BYTES {
        members.pop();
    }
    members.join(",")
}

fn is_allowed(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
        && value.is_ascii()
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use opentelemetry::propagation::{Extractor, Injector};

    use super::{
        Carrier, TRACEPARENT, TRACEPARENT_MAX_BYTES, TRACESTATE, TRACESTATE_LONG_MEMBER_BYTES,
        TRACESTATE_MAX_BYTES,
    };

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
    fn a_refused_tracestate_keeps_the_parent() {
        for state in [
            "not-a-list-member".to_owned(),
            "vendor=a\n".to_owned(),
            format!("vendor={}", "a".repeat(TRACESTATE_MAX_BYTES - 6)),
        ] {
            let carrier = Carrier::from_stored(Some(PARENT), Some(&state)).expect("parent kept");
            assert_eq!(carrier.get(TRACEPARENT), Some(PARENT));
            assert_eq!(carrier.get(TRACESTATE), None);
        }
    }

    #[test]
    fn rejects_non_ascii_control_and_overbound_parents() {
        assert!(Carrier::from_stored(Some("parent\n"), Some("vendor=a")).is_none());
        assert!(Carrier::from_stored(Some("parenté"), None).is_none());
        assert!(
            Carrier::from_stored(Some(&"a".repeat(TRACEPARENT_MAX_BYTES + 1)), None,).is_none()
        );
    }

    #[test]
    fn an_overbound_captured_tracestate_loses_long_then_trailing_members() {
        let long = format!("long={}", "l".repeat(TRACESTATE_LONG_MEMBER_BYTES));
        let short: Vec<String> = (0..60).map(|index| format!("v{index:02}=abcd")).collect();
        let captured = Carrier {
            trace_parent: Some(PARENT.to_owned()),
            trace_state: Some(format!("first=1,{long},{}", short.join(","))),
        }
        .validated();

        let state = captured.get(TRACESTATE).expect("state kept");
        assert!(state.len() <= TRACESTATE_MAX_BYTES, "{}", state.len());
        assert!(state.starts_with("first=1,v00=abcd,"), "{state}");
        assert!(!state.contains("long="), "{state}");
        assert!(!state.contains("v59="), "{state}");
        assert_eq!(captured.get(TRACEPARENT), Some(PARENT));
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
