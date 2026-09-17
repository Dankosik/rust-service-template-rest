---
name: rust-observability
description: "Operator evidence. Use when a Rust service log field, span, metric, or probe must answer an operational question, or when an emitted field changes correlation, cardinality, privacy, or cost."
---

# Rust Observability

**Operator evidence.** Start with the operational question and choose the smallest signal that answers it; every emitted field is either evidence or avoidable cost. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Logs are tracing events with structured fields and a stable snake_case event name an operator can search, never formatted sentences. Correlation is already carried: every record inside a request has the request id, trace id, and span id from the span the hardened chain opened, so do not thread identifiers by hand or log them twice. The process installs exactly one subscriber in the telemetry crate; library crates never install one and never configure levels.

Metrics go through the metrics facade with a describe call beside the first use and snake_case names ending in total for counters. Labels are a cardinality budget: use route templates with an explicit unmatched label, finite outcomes, and problem codes; never a raw path, a user id, a peer address, or an error message. HTTP server, process, and Tokio runtime metrics already exist; add an instrument only for a question they cannot answer, and keep the diagnostics listener serving the metrics route alone.

Spans for HTTP come from the tracing layer in the chain; inner operations use the instrument attribute and record identifiers and outcomes as fields rather than payloads. Every field is a disclosure surface: no secrets, tokens, bodies, or personal data. The trace exporter is installed only when an endpoint resolves, and its startup state is a configuration signal, not delivery health; a successful local emission proves nothing about a collector.

Probes are control inputs rather than diagnostics. Liveness describes process progress; readiness is the cached verdict the health crate refreshes in the background with a failure threshold and a staleness guard. A new dependency contributes a probe and a readiness consequence, never a per-request check, and its probe must respect the evaluation budget.

For review, account for every affected signal's question, labels, readers, and privacy without editing. For implementation, verify the signal's output and cardinality with a focused test or a scrape of the metrics route, and distinguish local emission from deployment evidence. Do not add dashboards, alerts, or a lifecycle audit because a skill mentions them.
