---
name: rust-observability
description: "Operator evidence. Use when logs, spans, metrics, or probes change operational visibility, correlation, cardinality, privacy, or cost."
---

# Rust Observability

**Operator evidence.** Start with the question an operator must answer and choose the smallest signal that distinguishes the relevant states. Identify who consumes it, what its absence means, and the cost and disclosure of producing it.

`crates/infra-telemetry` and `crates/infra-http` govern admitted fields, event names, subscriber installation, and request correlation. Library code emits through the selected facade rather than installing a global subscriber; preserve the distinction between diagnostics and intentional CLI output.

Give events stable identities and useful structured fields. An error's source chain or a span's existence does not guarantee that the renderer or exporter exposes it, so inspect the final signal the operator receives and avoid duplicate reporting that makes one failure look like several. Carry correlation through the existing context, and check background and handoff paths where it may be absent rather than claiming all records inherit request identity.

Metrics describe populations, so choose a unit, aggregation, and bounded set of labels that answer the question. Route templates, finite outcomes, and stable problem codes can be useful; raw paths, arbitrary errors, and user identifiers create cardinality and disclosure risks. Account for retention, exporter cost, and work on hot paths, and reuse an existing instrument when it already distinguishes the state rather than adding labels for merely available data.

Logs, spans, metric labels, errors, and `Debug` formatting are all disclosure surfaces. Apply redaction and field admission before publication, including failure paths and third-party diagnostics, and avoid capturing complete request or response payloads by default. Locally emitted events prove local emission, not collector delivery, alerting, or retention; keep those claims separate.

Probes are control inputs: determine liveness, readiness, startup admission, and dependency criticality with their lifecycle and integration owners in `crates/service/src/bootstrap`. A dependency that supports degradation need not gate readiness (cache and object-storage profiles opt in only when composition requires it). Where a probe participates, preserve its budget, cached verdict, and staleness policy rather than performing a new remote check on each request.

Do not add dashboards, alerts, probes, a collector, or a lifecycle audit solely because this method names them.
