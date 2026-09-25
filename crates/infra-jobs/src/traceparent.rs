//! The `traceparent` text stored on a job and linked from its attempt span.

use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// `00-{trace_id}-{span_id}-{flags}` when `context` is valid.
pub(crate) fn format(context: &SpanContext) -> Option<String> {
    if !context.is_valid() {
        return None;
    }
    let trace_id = context.trace_id();
    let span_id = context.span_id();
    let trace_flags = context.trace_flags();
    Some(format!(
        "00-{trace_id:032x}-{span_id:016x}-{trace_flags:02x}"
    ))
}

/// A `traceparent` of exactly 55 bytes: version `00`, lowercase hex, neither id zero.
pub(crate) fn parse(text: &str) -> Option<SpanContext> {
    let bytes = text.as_bytes();
    if bytes.len() != 55 || bytes[2] != b'-' || bytes[35] != b'-' || bytes[52] != b'-' {
        return None;
    }
    if &bytes[0..2] != b"00" {
        return None;
    }
    if !bytes[3..35]
        .iter()
        .chain(bytes[36..52].iter())
        .chain(bytes[53..55].iter())
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let trace_id = TraceId::from_hex(&text[3..35]).ok()?;
    let span_id = SpanId::from_hex(&text[36..52]).ok()?;
    let flags = u8::from_str_radix(&text[53..55], 16).ok()?;
    let context = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(flags),
        true,
        TraceState::default(),
    );
    context.is_valid().then_some(context)
}

/// The current span's `traceparent`, when that context is valid.
pub(crate) fn current() -> Option<String> {
    let cx = tracing::Span::current().context();
    format(TraceContextExt::span(&cx).span_context())
}

/// Link `span` to a stored `traceparent`. Nothing happens when `text` does not parse.
pub(crate) fn link(span: &tracing::Span, text: &str) {
    if let Some(context) = parse(text) {
        span.add_link(context);
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::SpanContext;

    use super::{current, format, parse};

    const EXAMPLE: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn example_round_trips() {
        let parsed = parse(EXAMPLE).unwrap();
        assert_eq!(format(&parsed).as_deref(), Some(EXAMPLE));
    }

    #[test]
    fn flags_zero_round_trip() {
        let text = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00";
        let parsed = parse(text).unwrap();
        assert_eq!(format(&parsed).as_deref(), Some(text));
    }

    #[test]
    fn empty_context_formats_none() {
        assert!(format(&SpanContext::empty_context()).is_none());
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse("00-4BF92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none());
        assert!(parse("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none());
        assert!(parse(&EXAMPLE[..54]).is_none());
        assert!(parse(&format!("{EXAMPLE}0")).is_none());
        assert!(parse("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none());
        assert!(parse("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01").is_none());
        assert!(parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g").is_none());
        assert!(parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-+1").is_none());
    }

    #[test]
    fn current_without_otel_span_is_none() {
        assert!(current().is_none());
    }
}
