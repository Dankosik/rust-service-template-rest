# Intent: production telemetry hardening

Status: ready

## Problem

The completed, independently reviewed TELEM-R2 research found preventable
availability, privacy, and reporting weaknesses in the current telemetry path.
It established source-level behavior, not measured production incidents. The
requester asked to implement all recommendations judged necessary for this
project in one separate pull request.

## Desired outcome

The shipped telemetry path preserves application availability when a log sink
stops consuming, bounds its retained log data, withholds unsafe diagnostic
data, and reports export and shutdown outcomes without implying delivery it
cannot establish. Operators can distinguish observable loss and failure from
normal local completion and understand the remaining delivery limits.

## Affected actors and systems

Public HTTP/gRPC callers, service operators, and the existing service, worker, and
migration entrypoints that consume the shared telemetry API; local JSON/text
logging, OTLP traces, the existing Prometheus recorder, and process teardown.
Collector and backend operators consume the documentation but their running
systems are outside this change.

## Scope and non-goals

One PR covers the necessary telemetry behavior changes, focused negative proof,
and updates to existing operating documentation. Preserve unrelated work.
Do not redesign dashboards, install infrastructure, deploy or merge, run paid
or live backend experiments, add another telemetry pipeline, or broaden this
into a performance or repository-wide observability project.

## Constraints

Keep the stock OpenTelemetry pipeline and useful existing custom formatting,
correlation, credential, TLS, and configuration protections. Mechanism,
dependency admission if needed, limits, and proof implementation are agent-owned
technical decisions. Existing sampling ratios, histogram buckets, and unrelated
features are unchanged. No new user-owned question survives Intake.

Authority covers scoped repository edits, local validation, dependency admission
when justified, commit/push, and a separate pull request. It does not cover merge,
deployment, external infrastructure changes, or paid/live backend runs. This
actor stops after reviewed Definition; the root continues the later phases.

## Success signal

The separate PR implements the accepted behavior in [the specification](spec.md),
passes its applicable local and CI gates, and reports exactly what was proved
and what remains deployment policy. Source analysis and old benchmarks are not
presented as current runtime or production-delivery evidence.
