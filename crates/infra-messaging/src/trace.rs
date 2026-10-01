//! W3C trace context in NATS headers, as the Go template writes and reads it.

use async_nats::{HeaderMap, HeaderName};
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::TraceContextExt as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

pub(crate) const TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");
pub(crate) const TRACESTATE: HeaderName = HeaderName::from_static("tracestate");
const TRACEPARENT_MAX_BYTES: usize = 256;
const TRACESTATE_MAX_BYTES: usize = 512;

/// Writes `span`'s context into a publication's headers. A span that is not
/// recorded writes nothing.
///
/// Only the two trace headers are admitted, and at most 768 bytes of them, so
/// the five identity headers plus these stay far below the 8 KiB header limit.
pub(crate) fn inject(span: &tracing::Span, headers: &mut HeaderMap) {
    let context = span.context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut Publication(headers));
    });
}

/// Makes the publisher's span the parent of `span`, which must not have been
/// entered yet. A delivery without a valid trace context starts its own trace.
pub(crate) fn set_remote_parent(span: &tracing::Span, headers: &HeaderMap) {
    let context = global::get_text_map_propagator(|propagator| {
        propagator.extract_with_context(&opentelemetry::Context::new(), &Delivery(headers))
    });
    if context.span().span_context().is_valid() {
        // Without an OpenTelemetry layer there is no span to attach to.
        let _ = span.set_parent(context);
    }
}

struct Publication<'a>(&'a mut HeaderMap);

impl Injector for Publication<'_> {
    fn set(&mut self, key: &str, value: String) {
        let (name, max_bytes) = match key {
            "traceparent" => (TRACEPARENT, TRACEPARENT_MAX_BYTES),
            "tracestate" => (TRACESTATE, TRACESTATE_MAX_BYTES),
            _ => return,
        };
        // A header value is written to the wire as is, so it must not carry a
        // line break.
        if !value.is_empty()
            && value.len() <= max_bytes
            && value
                .bytes()
                .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
        {
            self.0.insert(name, value.as_str());
        }
    }
}

struct Delivery<'a>(&'a HeaderMap);

impl Extractor for Delivery<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        let name = match key {
            "traceparent" => TRACEPARENT,
            "tracestate" => TRACESTATE,
            _ => return None,
        };
        self.0.get(name).map(async_nats::HeaderValue::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        ["traceparent", "tracestate"]
            .into_iter()
            .filter(|key| self.get(key).is_some())
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed valid fixtures")]
mod tests {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    fn with_tracing<T>(test: impl FnOnce() -> T) -> T {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let provider = SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        tracing::subscriber::with_default(subscriber, test)
    }

    fn trace_id(span: &tracing::Span) -> opentelemetry::trace::TraceId {
        span.context().span().span_context().trace_id()
    }

    #[test]
    fn a_delivery_span_continues_the_trace_of_the_publication_span() {
        with_tracing(|| {
            let publish = tracing::info_span!("publish");
            let mut headers = HeaderMap::new();
            inject(&publish, &mut headers);

            let traceparent = headers.get(TRACEPARENT).unwrap().as_str();
            assert!(traceparent.contains(&trace_id(&publish).to_string()));

            let process = tracing::info_span!("process");
            set_remote_parent(&process, &headers);
            assert_eq!(trace_id(&process), trace_id(&publish));
        });
    }

    #[test]
    fn a_delivery_without_a_valid_trace_context_starts_its_own_trace() {
        with_tracing(|| {
            for traceparent in [None, Some("not-a-traceparent")] {
                let mut headers = HeaderMap::new();
                if let Some(value) = traceparent {
                    headers.insert(TRACEPARENT, value);
                }
                let process = tracing::info_span!("process");
                set_remote_parent(&process, &headers);
                assert_ne!(
                    trace_id(&process),
                    opentelemetry::trace::TraceId::INVALID,
                    "{traceparent:?}"
                );
            }
        });
    }

    #[test]
    fn an_unrecorded_span_adds_no_header() {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let mut headers = HeaderMap::new();
        inject(&tracing::Span::none(), &mut headers);
        assert!(headers.is_empty());
    }

    #[test]
    fn injection_admits_only_bounded_single_line_trace_headers() {
        let mut headers = HeaderMap::new();
        let mut publication = Publication(&mut headers);
        publication.set("baggage", "account=123".to_owned());
        publication.set("tracestate", "vendor=a\r\nNats-Msg-Id: forged".to_owned());
        publication.set("traceparent", "a".repeat(TRACEPARENT_MAX_BYTES + 1));
        assert!(headers.is_empty());

        Publication(&mut headers).set("tracestate", "vendor=a".to_owned());
        assert_eq!(headers.get(TRACESTATE).unwrap().as_str(), "vendor=a");
    }
}
