---
name: rust-observability
description: "Operator evidence. Use when a Rust log, span, metric or probe must distinguish operational outcomes, or when correlation, cardinality, privacy or emission cost is unresolved."
---

# Rust Observability

Name the operational decision the signal supports before choosing its format.
An operator may need to distinguish no work, successful work, failed work,
degraded service and unknown completion; one success counter or error string
can erase those distinctions. Reuse a signal that already answers the question
before creating a second instrument with a competing meaning.

## Match evidence to the question

A metric summarizes a population; an event explains an occurrence; a span
connects work across an operation. Choose the smallest combination that supports
the decision. A duration needs a known start and end: queue wait, execution,
response headers and completed delivery are different intervals. Report partial
or cancelled work under its actual observation rather than calling elapsed
wait time successful completion.

Keep correlation with the owner of the operation and carry its context across
execution boundaries. Repeating identifiers in every function parameter or
log field can diverge from that context. A task deliberately detached from a
request needs its own lifetime and causal relation, not a misleading request
span that appears to have finished all its work. Correlation is diagnostic
custody, never authentication evidence.

Labels are a storage and query-cost decision. Their combinations multiply:
several individually finite dimensions can still create an expensive population.
A route template and finite outcome usually answer aggregate questions; a raw
path, user identifier or error message creates attacker-controlled series and
possible disclosure. Keep occurrence-specific detail only where the question
and privacy policy justify it, with bounded representation. A stable hash of a
secret or identifier does not make it safe or low-cardinality.

## Separate observation from control

A readiness verdict changes routing; liveness can trigger process replacement.
Choose the consequence of a dependency failure before adding it to a probe.
An optional dependency may justify degraded operation, while a dependency
required for every useful request may justify removing the instance from
service. Making all replicas unready for a shared outage can also remove useful
fallbacks. Apply the accepted lifecycle policy rather than adding readiness for
every client object. Probe evaluation itself consumes time and dependency
capacity; a diagnostic signal is not automatically a safe control input.

Local emission, exporter completion, collector receipt and durable backend
storage are separate evidence boundaries. A configured exporter does not prove
receipt, and a missing sample is not necessarily zero. Account for filtering,
sampling, queue loss and truncation before drawing a conclusion from absence.

For changed event/metric naming, subscriber ownership, correlation or disclosure,
load [Logging policy](../../../docs/configuration-source-policy.md#logging).
For exporter and diagnostics-listener policy, load [OpenTelemetry Environment
Policy](../../../docs/configuration-source-policy.md#opentelemetry-environment-policy).
For probe behavior and telemetry startup/teardown custody, load [Runtime
Lifecycle](../../../docs/architecture/runtime-lifecycle.md). These owners preserve
local conventions; library defaults do not replace them. An unresolved signal
question alone does not require dashboards, alerts or a deployment audit.
