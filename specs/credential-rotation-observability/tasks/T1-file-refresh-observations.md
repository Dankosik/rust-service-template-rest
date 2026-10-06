# T1 — Truthful file-refresh observations

Outcome:
Replace the absence of consistent completed-operation observations with the
three bounded R1 counter families at the existing PostgreSQL, Valkey and NATS
file owners, without changing their control flow or authentication policy.

Consumes:
- [Specification R1](../spec.md#r1-bounded-observations-at-the-actual-file-refresh-boundary)
  and [composition](../spec.md#composition-and-compatibility) — observable meaning.
- [Design metric schema](../design/design.md#metric-schema-and-material-flows)
  and its PostgreSQL, Valkey and NATS subsections — exact schemas and writers.

Provides:
- Completed outcome counters with finite labels, matching owner coverage and
  documentation distinguishing options installation, accepted Valkey AUTH and
  prepared NATS challenges. T3/T4 consume these installed observations.

Boundary:
Keep each family and its tests with the current provider owner. Preserve
existing log boundaries, repeated-failure suppression, schedules, cancellation,
generation retirement and startup admission. Do not add authentication claims,
network exchanges, new dependencies, shared credential abstractions, config or
runtime tasks. Existing PostgreSQL/LISTEN coverage is reused at its scope.

Mutable owners:
- `crates/infra-postgres/src/credentials.rs` and its existing owner tests.
- `crates/infra-cache/src/connection.rs`, the existing bounded mapping/helper
  owner `src/observe.rs` only as Design permits, and colocated owner tests.
- `crates/infra-messaging/src/credentials.rs` and its existing owner tests.
- `docs/architecture/persistence.md`, `docs/cache.md`,
  `docs/durable-messaging.md`: corresponding signal/event explanations.

Exclusive locks:
- File-refresh production owners above; their three guide files. These are
  released on Implemented before T3/T4 guide edits begin. The Lead may use
  disjoint provider sublanes; it owns their integration and join.

Final validation:
- Claim: Every completed owner outcome has the exact R1 semantics and bounded
  schema; no observation changes existing failure, cancellation or policy.
- Checks: Shared local completion route and final delivery review in
  [tasks.md](../tasks.md); executor chooses focused tests and commands. No new
  PostgreSQL runtime claim or duplicate real-database proof is added.
- Observable: Owner-level outcome samples and retained event boundaries agree
  with actual completion, including failed and unchanged work; secret data
  cannot create output values or unbounded label series.

Reopen if:
Specification owns changed signal meaning; Design owns a necessary different
emission owner or flow; Research owns contradictory resolved API/base evidence.
Routine test/oracle and implementation repairs remain in this unit.
