# Specification: webhook payload overhead

Status: ready. Independent [Specification review](definition-review.md): PASS.
Authority: [Intent](intent.md); [accepted profiling](../hotpath-profiling/report.md)
and its [review](../hotpath-profiling/review.md).

## Outcome and selected scope

Reduce avoidable body-dependent allocations and preparation work between
accepting an inbound webhook body and inserting its durable job. The primary
success metric is allocated bytes per newly admitted delivery: for the retained
64 KiB scenario, reduce measured payload construction/preparation turnover by
at least one body-length equivalent relative to the fixed baseline, with
equivalent profiling and scope. This is an acceptance target, not a measured
result or a promised latency percentage. Report allocation counts separately.

This scope is chosen because the report measures about 64.1 KiB in
`Incoming::new` and 342 KiB in `enqueue::prepare` for its large-body case.
The predicted 3–8% flow CPU improvement is not an acceptance promise. Technical
Design owns mechanisms, API choices, and placement; this contract selects none.

## Deliberately unchanged behavior

Current source and its existing contracts remain canonical:
[HTTP admission](../../crates/infra-http/src/webhooks.rs),
[inbound webhook](../../crates/infra-webhooks/src/inbound.rs),
[job enqueue](../../crates/infra-jobs/src/enqueue.rs),
[async architecture](../../docs/architecture/async.md), and
[persistence architecture](../../docs/architecture/persistence.md).
The implementation baseline must fix these bytes before optimization; unrelated
dirty work is part of that baseline, not an optimization delta.

| Surface / trigger | Required result and forbidden divergence |
| --- | --- |
| Valid new delivery | Preserve HTTP status, response headers/body bytes, identifiers, receipt contents, job kind/payload, and consumer-visible body bytes. Stored payload serialization before PostgreSQL normalization stays byte-identical, including the existing Base64 representation. |
| Signature and parsing | Authenticate exactly the same original body bytes with the same verification, timestamp, header, and parsing rules. Preserve errors and rejection precedence; rejected input gains no durable effect. |
| Duplicate delivery or concurrent identical delivery IDs | Preserve first-admission arbitration, deduplication scope and retention, response behavior, and the winning payload. Repeated admission cannot create another job, replace the first payload, or add preparation work merely to bypass the current deduplication decision. |
| Durable admission | Receipt and job commit atomically in the caller's transaction. Preserve transaction owner, isolation, known-failure/rollback behavior, and commit-unknown semantics. Unknown acknowledgement is never reported as certain success and never causes automatic replay of the business closure. |
| Bounds and resource failure | Preserve body and serialized-payload limits, validation/error identity and precedence, admission/concurrency limits, deadlines, overload responses, and shutdown behavior. Additional retained capacity must remain bounded by existing limits; a request cannot leave unbounded or process-lifetime payload storage. |
| Logs, traces, metrics | Preserve current logging decisions, error observations, correlation, trace propagation, labels and cardinality bounds. Saved work must come from payload handling, not removal of observability. |
| Other jobs and HTTP routes | Shared preparation changes preserve all existing job serialization/validation semantics and trace carriers. Unrelated HTTP routes, optional-profile behavior, and public contracts remain unchanged. |

Representative composition: two valid simultaneous deliveries with the same ID
but different bodies still establish only the currently permitted first receipt
and matching job. A retry after an uncertain commit follows existing deduplication
and failure behavior. None of these paths may trade durable truth for lower
allocation or latency.

## Proof and success meaning

The final comparison uses a fixed baseline and fixed optimized source identity
on the same approved new droplet. Match toolchain, resolved dependencies,
features, release profile, allocator, CPU placement, database, load generator,
fixtures, pool size, warmup, and instrumentation within each comparison.
Historical measurements justify the scope but cannot replace this new control.
Keep pool settings equal between baseline and final; any 16-connection cell is
an explicitly labeled synthetic control, never a new global default.

Measure ordinary release behavior separately from matched instrumented
allocation observations. Compare new small and large webhook deliveries,
duplicate-only traffic, and the retained mixed workload. Record service CPU,
latency percentiles, achieved successful throughput, failures, dropped work,
and peak RSS, alongside the primary allocation metric. Retain repeated samples
and their spread; do not select only favorable runs or count fast rejections
as useful throughput. Test selection, exact commands, and sufficient repetition
belong to Implementation under repository validation policy.

Acceptance requires the primary allocation target, relevant passing behavior
proof, and no reproducible regression beyond observed run-to-run variation in
CPU, latency, useful throughput, errors/drops, or peak RSS for these comparison
workloads. Behavioral proof must cover the affected serialization and admission
invariants, including real PostgreSQL proof for claimed durable behavior under
the persistence validation owner. Reuse adequate existing tests. Tests and
measurements run only remotely under [Intent's constraints](intent.md#constraints).
No local validation command is implied by this specification.

If the apparent effect is inconclusive, narrow or remove the unsupported
change; do not declare success from forecasts. Technical Design/Implementation
may choose the simpler correct candidate within this outcome. If no candidate
can meet it, reopen Definition with evidence rather than silently accepting
extra complexity or expanding to another bottleneck. Implementation completion
still requires a new comparison report and the applicable final independent
review; this phase receipt proves neither implementation nor runtime success.

## Exclusions and reopen conditions

- Keep `postgres.max_connections` default 4 and its operator-owned global
  connection budget. The earlier 16-slot synthetic result is not general
  production capacity evidence.
- Do not change SQL round trips, transaction topology, durability, schema,
  worker behavior, notification locks, storage, allocator, release profile,
  dependencies, or HTTP telemetry in this candidate.
- Production SLO, Railway performance, long-duration leak freedom, and provider
  performance are not claims this synthetic comparison can establish.
- Reopen Definition for changed observable behavior, scope, or primary success
  meaning; reopen Technical Design for mechanism/ownership conflicts; return
  unavailable infrastructure or external authority to the root coordinator.

Open user-owned questions: none.
