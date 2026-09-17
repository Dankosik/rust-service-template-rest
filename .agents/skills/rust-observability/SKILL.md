---
name: rust-observability
description: "Use when an operational question needs a log field, span, metric, or probe, or when an emitted field or label changes correlation, cardinality, privacy, or cost."
metadata:
  invocation: model
  kind: method
---

# Rust Observability

Telemetry is **operator evidence**: every signal answers a named operational
question or is avoidable cost.

`operator question -> signal -> correlation -> cardinality, privacy, cost -> proof`

Logs are `tracing` events with structured fields (`tracing::info!(key = %value,
"event_name")`), never formatted strings; the event name is a stable
snake_case token an operator can search. Correlation is free: every record
inside a request carries `request_id`, `traceId`, and `spanId` from the span
the `harden` chain opened, so do not thread ids by hand or log them again.
The process installs exactly one subscriber in `infra_telemetry::logging`;
library crates never install one.

Metrics go through the `metrics` facade (`counter!`, `gauge!`, `histogram!`)
with a `describe_*!` beside the first use and a snake_case name ending in
`_total` for counters. Labels are a cardinality budget: use route templates
(`endpoint`, with `<unmatched>` for fallbacks), finite outcomes, and problem
codes; never a raw path, user id, peer address, or error message. HTTP
server, process, and Tokio runtime metrics already exist; add an instrument
only for a question they cannot answer.

Spans come from `axum-tracing-opentelemetry` for HTTP and from
`#[tracing::instrument]` for inner operations; record ids and outcomes as
fields, not payloads. Every emitted field is a disclosure surface: no
secrets, tokens, bodies, or personal data.

Probes are control inputs, not diagnostics: liveness is process-only,
readiness is the cached verdict in `crates/health` refreshed in the
background. A new dependency adds a `Probe` and a readiness consequence,
never a per-request check. The diagnostics listener serves `/metrics` only
and stays off the application listener.

For review, account for every affected signal's question, labels, and
readers. For implementation, verify the signal's output and cardinality with a
focused test or a scrape of `/metrics`; local checks describe emission, not
delivery to a collector.
