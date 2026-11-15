---
name: rust-observability
description: "Operator evidence. Use when logs, spans, metrics, or probes change operational visibility, correlation, cardinality, privacy, or cost."
---

# Rust Observability

**Operator evidence.** Start from the operational question an operator must answer and emit the smallest signal that separates the relevant states, accounting for its consumer, absence meaning, hot-path cost, and privacy boundary.

**Telemetry owners.** Emit library diagnostics through `tracing` and `metrics` facades while keeping subscriber installation, field admission, and HTTP correlation in `crates/infra-telemetry` and `crates/infra-http`, and keeping intentional CLI output separate from diagnostic events.

**Events and correlation.** Give events stable names and structured fields, verify that the configured formatter or exporter surfaces error causes and span context, report each failure once at its owning boundary, and propagate correlation explicitly across background tasks and handoffs where ambient request spans end.

**Metrics cardinality.** Choose metric units, aggregations, and bounded label sets—route templates, finite outcomes, and stable Problem codes—reusing existing instruments where they already separate the state and keeping high-cardinality paths, raw errors, and user IDs out of labels.

**Redaction and claim scope.** Enforce field admission and secret redaction across logs, spans, metric labels, errors, and `Debug` output before emission, omitting raw request/response bodies by default and distinguishing local emission proof from collector delivery or alerting.

**Health probes.** Align liveness, readiness, startup admission, and dependency criticality with `crates/service/src/bootstrap`: gate readiness only on non-degradable dependencies, preserve cached probe budgets and staleness windows, and scope telemetry additions strictly to the requested signal.
