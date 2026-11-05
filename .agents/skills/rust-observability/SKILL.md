---
name: rust-observability
description: "Operator evidence. Use when a Rust log field, span, metric, or probe must answer an operational question, or when an emitted field changes correlation, cardinality, privacy, or cost."
---

# Rust Observability

**Operator evidence.** Start with the question an operator must answer and choose the smallest signal that distinguishes the relevant states. Identify who consumes it, what its absence means, and the cost and disclosure of producing it. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Use the project's existing logging, correlation, and export owner. In this service, [configuration and telemetry policy](../../../docs/configuration-source-policy.md) and the [HTTP architecture](../../../docs/architecture/http.md) govern admitted fields, event names, subscriber installation, and request correlation. Library code emits through the selected facade rather than installing a global subscriber. Preserve the distinction between diagnostics and intentional CLI output; a standalone command's stdout interface does not require a service telemetry stack.

Give events stable identities and useful structured fields. An error's source chain or a span's existence does not guarantee that the renderer or exporter exposes it, so inspect the final signal that the operator receives. Avoid duplicate reporting that makes one failure look like several. Carry correlation through the existing context, and check background and handoff paths where it may be absent rather than claiming all records inherit request identity.

Metrics describe populations, so choose a unit, aggregation, and bounded set of labels that answer the question. Route templates, finite outcomes, and stable problem codes can be useful; raw paths, arbitrary errors, and user identifiers create cardinality and disclosure risks. Account for retention, exporter cost, and work on hot paths. Reuse an existing instrument when it already distinguishes the state; a new label or metric needs an operational use, not merely available data.

Logs, spans, metric labels, errors, and debug formatting are all disclosure surfaces. Apply redaction and field admission before publication, including failure paths and third-party diagnostics. Avoid capturing complete request or response payloads by default. A locally emitted event proves local emission; exporter configuration is not collector delivery, and a successful scrape is not evidence that alerting or retention works. Keep those claims separate.

Probes are control inputs. Determine liveness, readiness, startup admission, and dependency criticality with their actual lifecycle and integration owners. A dependency that supports degradation need not gate readiness; this service's cache and object-storage profiles opt in only when composition requires it. Where a probe participates, preserve its budget, cached verdict, and staleness policy rather than performing a new remote check on each request. Use the [runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md) and [integration architecture](../../../docs/architecture/integration.md) for the selected profile.

For review, connect each affected signal to its question, readers, cardinality, and privacy without editing. For implementation, use focused existing proof or an authorized observation of the rendered output, and distinguish local from deployment evidence. Do not add dashboards, alerts, probes, a collector, or a lifecycle audit solely because this method names them.
