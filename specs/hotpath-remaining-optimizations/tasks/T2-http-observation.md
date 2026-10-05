# T2 — Preserve HTTP signals with bounded borrowed representations

Outcome:
Replace avoidable span/metric text ownership with the accepted private borrowed
representations, preserving every emitted span, log and metric value and its
current request/response-body lifecycle.

Consumes:
- [Specification](../spec.md#preserved-behavior-and-truth) — signal, HTTP and resource obligations.
- [HTTP design](../design.md#http-span-and-metric-mechanisms) and [ownership](../design.md#ownership-map) — method/protocol/name/status mechanisms and independent rejection rule.
- [Ledger baseline custody](../tasks.md#carrier-and-ready-frontier) — immutable pre-edit source.

Provides:
- An independently attributable HTTP observation delta with separate span/metric and name-formatting dispositions, plus material gaps covered in existing-owner tests.

Boundary:
Use static standard method values with owned extension methods, the existing
protocol Cow, and the fixed typed StatusCode table for 100..999 labels. Evaluate
display formatting of the span name with exact original trim/separator meaning;
it remains independently removable. Preserve labels/series/buckets, route
ownership, correlation, parent/export/filter decisions, refusal visibility and
active-body lifetime. No cache, lock, recorder/subscriber/router policy,
manifest, configuration, public API or new module. Implementation chooses and
writes tests with the code, preserving unrelated existing edits.

Mutable owners:
- HTTP span/metric representation and existing observation/hardening/context/log/metric tests within `crates/infra-http/src/observe.rs`.

Exclusive locks:
- none

Final validation:
- Claim: the assembled retained changes preserve full signal meaning and lifecycle while qualifying under the span/metric allocation-or-CPU adoption rules.
- Checks: consolidated ledger Completion; matching build/tests, complete numeric label and emitted-value parity, instrumented span/metric attribution, and ordinary SQL-free HTTP controls. Implementation owns concrete tests/commands; no T2-specific test/review pause.
- Observable: span and metric results are reported separately, with allocation bytes/count and CPU distinguished; ordinary HTTP useful throughput/errors/drops, CPU, latency and peak RSS show no reproducible regression. Failed name formatting or other candidates are independently removed with unfavorable evidence retained.

Reopen if:
The supported APIs cannot preserve complete values/lifetimes or a useful
candidate needs caching/new responsibility: Technical Design. Changed signal
meaning/adoption rules: Definition. Missing required remote proof: root/delivery
at Completion, without holding independent implementation.
