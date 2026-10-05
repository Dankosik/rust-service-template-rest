# T2 — Bounded private telemetry through terminal cleanup

Outcome:
Replace synchronous/unbounded local logging and misleading shutdown reporting
with the accepted bounded private pipeline and truthful terminal observation,
owned through every existing consumer's normal, failed and interrupted startup
and shutdown. Historical loss remains observable without replaying business work.

Consumes:
- T1 integrated implementation: admitted inbound observations and its documentation
  baseline; dependency gates implementation, not passing checks.
- [S1](../spec.md#s1-truthful-export-and-shutdown-reporting),
  [S2](../spec.md#s2-diagnostic-privacy-at-the-source),
  [S3](../spec.md#s3-bounded-logging-that-fails-open) and
  [S4](../spec.md#s4-operating-contract-and-compatibility): remaining complete contract.
- [Design](../design/technical-design.md): selected mechanism/dependencies,
  local bounds, writer ownership, trace completion, global diagnostic admission,
  consumers/budgets and proof/operating boundary. Every section is consumed;
  numerical limits and exit/deadline choices are already closed.
- [Ownership responsibilities](../design/ownership.md#responsibilities) L/W/D/T/M,
  all P and C, [file map](../design/ownership.md#files), and
  [component evidence](../design/component-evidence.md): current source owners,
  extension APIs, all public API consumers and replacement obligations.

Provides:
- Shared bounded JSON/text capture, one standard-library writer with non-waiting
  bounded admission, independent finite loss accounting, runtime-independent
  logger shutdown and a destructor that never waits.
- Global numeric-only SDK diagnostic observation before both local/OTel outputs;
  raw denied-family diagnostics never pass, including bridged log origin targets.
  Payload-free shared panic hook and HTTP recovery, with no obsolete
  `PanicMessage` consumers; the existing sanitized 500 Problem remains.
- Truthful trace final-drain latch (including already-in-flight and inner shutdown
  failures), `Completed`/finite `Incomplete` results and consumer exit mapping;
  no `Flushed`/misleading event compatibility branch.
- Complete operating docs for limits, removed fields/events, local evidence and
  Collector/backend/deployment responsibilities, retaining historical cost evidence.

Boundary:
Use the selected stock SDK processor/retries and current dependencies. Rename
`logging/json.rs` into bounded `logging/format.rs`; create only the accepted
`logging/output.rs` and `logging/diagnostics.rs` owners. No legacy formatter path,
second telemetry pipeline, new config key, public privacy framework, manifest,
feature, lockfile, OpenAPI/protobuf, histogram or sampling change.

The producer-to-queue-to-writer byte/lifetime accounting and reentrancy/lock
rules in Design are one invariant: 16 KiB full record, 512 records/8 MiB queue,
32 callbacks with two 16 KiB buffers and 128 fields, 1024 cached spans with
4 KiB/64 fields, one 16 KiB writer record. Capacity and replacement scratch count;
source-owned formatting/SDK/registry/RSS exclusions are not weakened or renamed
as a process-memory guarantee.

All actual consumers are inside the unit: service normal/failed/interrupted
startup and teardown, ordinary worker prepare/abort/run, migration entrypoint
including no-runtime cleanup, and the existing isolated panic-hook test binary.
Finite worker commands, openapi and pre-telemetry CLI/config failures keep their
current output/exit behavior without installing telemetry. Template profile
markers/imports remain coherent. Service/ordinary worker use final-incomplete 3
unless primary failure requires 1; migration preserves primary 0/1 and exactly
one business terminal record, with no telemetry-triggered replay.

Keep the existing shared 5s telemetry tail and 17s aggregate tail; reserve up to
1s for logger closure and give trace at most 4s including deducted join slack.
Use one absolute deadline. Logger shutdown itself requires no runtime; final
runtime cap remains 1s within remaining process grace. Migration's existing 1s
cleanup allowance covers runtime termination, terminal business record and drain.
Do not add synchronous post-install sink fallback or a second destructor wait.

Mutable owners:
- `infra-http` HTTP panic recovery and its existing proof; shared P stays
  together because the process-wide hook runs before HTTP catches the panic.
- `infra-telemetry` logging/format/output/diagnostics, trace/export observation,
  metrics upkeep, public exports, current inline/delivery and isolated hook proof.
- Service bootstrap/shutdown and existing lifecycle proof; worker bootstrap,
  shutdown/lib and existing process proof; migrate entrypoint/result/cleanup.
- `docs/configuration-source-policy.md`, `docs/architecture/runtime-lifecycle.md`,
  `docs/infra-telemetry-performance.md` at their current owners.
- Existing `scripts/ci/migration-validate.sh` only if terminal-record assertions
  need adaptation. No SQL/migration/business effect change or new database harness.

Exclusive locks:
- HTTP recovery source; shared telemetry API and subscriber/global diagnostic/metrics composition;
  each binary's bootstrap/shutdown resource ownership; existing migration
  validator and named operating documents when changed. Leads partition these
  owners before delegating; declaration/consumer edits and shared process-global
  fixtures cannot have overlapping writers. Heavy validation is serial.

Final validation:
- Claim: complete S1-S4 at the bounded local formatter/export/process boundaries,
  with safe source admission, bounded blocked/failing output, finite independent
  loss/recovery evidence and truthful cleanup without changing primary effects.
- Checks: agreed behavior, matching build and relevant passing tests under the
  repository validation budget, docs consistency, no known in-scope defect and
  final independent review of the assembled candidate. Executor chooses concrete
  cases/commands at the [existing proof owners](../design/technical-design.md#proof-and-operating-boundary).
  Preserve the accepted negative proof and successful correlation/export
  compatibility; do not add a benchmark campaign or duplicate CI-owned migration
  gate locally. Applicable migration/profile/release checks remain their existing
  CI gates on the published PR head, not extra per-unit prerequisites.
- Observable: bounded retained producer/queue/writer state and caller/process
  termination with the sink still stopped at the assertion; actual JSON/text
  and OTLP privacy at configured debug/trace levels; final-drain failures survive
  later success; runtime-only losses do not falsely become failed business work.
  Entry points truthfully map final state and retain migration primary result.
  No final scrape, delivery/persistence, production incident or speedup is claimed.

Reopen if:
A fixed bound/lifecycle/API assumption fails: System Design. Ownership/placement:
Rust Ownership. Resolved SDK/provider behavior changes: Research, then affected
Design. Privacy/loss/exit semantics change: Specification. Changed user meaning
or external effect authority: Intake. Cases, fixtures, compile repair and scoped
validation commands remain Implementation decisions.
