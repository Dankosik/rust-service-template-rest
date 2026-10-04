# PostgreSQL pool resilience

Status: ready. Reviewed Definition after user steering, based on repository revision
`67be869acea112af271ec8ba621cbc50ae9d36b7`.

## Authority and baseline

[Intent](intent.md) owns the accepted outcome and effect authority.
[Persistence](../../docs/architecture/persistence.md) owns the current pool,
transaction and deployment contracts; [configuration
policy](../../docs/configuration-source-policy.md) owns existing budgets.

The current `infra-postgres` pool is SQLx 0.9.0, with a three-second acquire
budget, a one-second idle ping bound and configurable maximum connections
(default four). Its `DiscardOnDrop` protects pending `BEGIN`, then clears after
`BEGIN`; the driver return ping can wait for an answer after cancelled SQL.
The transaction wrapper records connection wait duration, while statements
executed directly through the pool and readiness use other acquisition paths.
The later bounded [driver probe](research/sqlx-release.md#bounded-local-experiment)
reproduced retention and demonstrated recovery through a timed owned-return
future; it did not test the selected five-second dependency backport. The
superseded facade choices in that research are historical, not active authority.
Relevant owners are [pool.rs](../../crates/infra-postgres/src/pool.rs),
[transaction.rs](../../crates/infra-postgres/src/transaction.rs),
[observe.rs](../../crates/infra-postgres/src/observe.rs), and
[probe.rs](../../crates/infra-postgres/src/probe.rs).

The user's [maintenance-simplicity steering](intent.md) reopens the earlier
assistant-selected SQLx exclusion, three-second cleanup assumption and universal
acquisition coverage. [Alternatives research](research/alternatives-reopen.md)
compares ready-made pooling against the full lifecycle and migration cost.
Library ownership of pooling is the accepted driver; do not introduce an
application `Pool` or `Executor` facade for this outcome. The current technical direction
is native SQLx with a narrowly scoped temporary dependency backport, whose
[source/custody proposal](design/dependency-custody.md) remains Technical Design's
responsibility. This is not a released upstream fix: the recorded upstream PRs
are unmerged/unreleased. Preserve provenance, review the isolated delta and its
delivery/profile custody, and state an objective retirement condition. A
contrary library or maintenance finding can reopen that technical direction;
retaining SQLx is not a user-owned restriction on alternatives.

## Behavior delta

### R1. Cancellation releases capacity without a network acknowledgement

When the caller drops or times out a template-owned pooled operation while
SQL or protocol I/O is unfinished, including transaction work and a pooled
autocommit statement or readiness ping, the abandoned connection must not
retain a pool slot indefinitely waiting for rollback, ping or close responses.
This applies during `BEGIN`, statements, pre-commit verification and `COMMIT`.
A network that accepts the connection but silently stops forwarding replies is
within scope; a TCP reset alone does not establish this behavior.

Capacity recovery must not depend on the abandoned peer becoming responsive.
The revised technical cleanup bound is a **five-second timeout around the whole
library-owned return operation**, including callback, ping and graceful-close
branches; expiry drops the abandoned ownership and releases its local slot.
This replaces the earlier three-second release assumption. Its basis is the
existing SQLx close-on-drop bound and the upstream-aligned whole-return fix,
not a user-supplied latency SLA. Pending-BEGIN disposal remains bounded and must
not reuse an unacknowledged open transaction. Cancellation while waiting for
acquisition must likewise release any locally owned permit or connection.

The ordinary acquire budget remains **three seconds**. A request arriving
during cleanup may exhaust that budget. The first immediately issued replacement
request is not promised success, and no automatic retry is added. Once cleanup
has released capacity and new connections can succeed, subsequent work can
acquire within the normal budget without restart or capacity growth. Repeated
cancellation must not accumulate slots beyond their bounded cleanup lifetime.
Keep the configured local pool limit; do not increase it as a workaround.

Local release is not a hard instantaneous bound on physical server sessions.
After a silently disconnected client is abandoned, its PostgreSQL backend may
remain until server/network cleanup even while a replacement connects. Do not
claim immediate backend termination or use local pool occupancy as its proof.

Cancellation does not prove server-side rollback or non-execution. Preserve the
existing commit-outcome classification: a lost or uncertain `COMMIT` outcome
must not become committed or safely retryable because its connection was
discarded. Do not add automatic SQL or transaction retries. A caller cancellation
still follows its existing deadline/error path; no new HTTP response is required.
Normal successful transactions and statements retain their observable results
and reusable healthy connections; discarding every healthy return is not the
selected remedy. A silence fault after successful SQL must not bypass the
whole-return bound merely because the statement already completed.

Nearest falsifier: cancel active SQL through the existing silenceable relay,
keep its old connection silent, allow replacement connections, and observe
capacity recovery after the cleanup window and successful unrelated work without
increasing pool size; an acquisition timeout during cleanup alone is not failure.
The existing transaction finality oracles remain applicable to affected paths.

### R2. Acquisition pressure is visible outside transactions

Operators must receive useful acquisition-specific diagnostics for slow success
and acquire-budget exhaustion at the named template operation paths:
transactions, observed direct statements (including startup/session and history
checks, jobs and maintenance), readiness, and other explicit adapter acquisition
callers. Technical Design identifies the existing callers and their observation
owner. A statement-duration signal alone is insufficient because it mixes
waiting and execution. Native slow-acquire logs plus ordinary acquisition
observation may jointly supply elapsed wait, PostgreSQL pool/operation context,
and success or timeout outcome; these need not be fields of one universal event.
Acquisition timeout must remain distinguishable from SQL execution failure and
pool closure. Arbitrary future external callers are not a universal interception
requirement.

Reuse existing signals and supported driver instrumentation where sufficient.
Retain the transaction wait histogram's meaning, and document the actual
coverage of native signals and operation observations, including the histogram's
transaction-only scope. Cancellation before
acquisition must not be reported as successful acquisition or a driver timeout
that did not occur; a universal cancellation metric is not required.
Use bounded fields and existing secret-redaction policy: no DSN, credentials,
request-derived labels or SQL parameters. Technical Design owns exact signal
names and slow-acquisition threshold with their existing configuration owner;
this task requires no new metric or operator knob, pool API, or public Executor
replacement. Application observation does not own pool return or connection
accounting.

Nearest falsifier: exhaust a small pool and compare transaction, observed direct
statement and readiness acquisition diagnostics, then free capacity and inspect
slow success. No duplicate counting of one acquisition as two attempts; native
logs and an operation observer describing the same attempt are complementary.

### R3. A complete connection-budget and sizing method

Operator guidance must account for peak simultaneously live service replicas,
jobs-worker replicas and their configured pools, rolling replacement overlap,
dedicated migrators and other direct connections, other applications, and
database administrative/reserved capacity. State the units and distinguish a
configured upper bound from average observed use. Include one worked example
whose allocations fit its stated total; example numbers are illustrative.
The allocated steady-state/local pool ceiling does not guarantee that abandoned
server sessions disappear before replacements arrive. Explain the transient
physical-session caveat and the role of server/pooler limits and reserve capacity.

For PgBouncer, distinguish application client connections from PostgreSQL
server connections; explain which configured pooler limits and database/user
partitions must be included instead of equating all client pools with backend
sessions. Preserve the existing worker minimum of `jobs.max_workers + 2` and
the migrator's required direct connection. When the budget cannot fit these
minimums, identify the conflict instead of recommending an invalid setting.

Sizing guidance must use observed acquisition waits/timeouts, pool occupancy,
request/job latency and database saturation/locks to distinguish too much
concurrency from too little pool capacity. A connection-count budget is a
ceiling, not a throughput-optimal setting. Retain the default of four and the
existing validation range. No production-optimal pool size is claimed.

Nearest falsifier: a worked deployment example omits a connection owner or
rollout overlap, confuses PgBouncer clients with backend sessions, exceeds its
server allowance, or recommends settings rejected by current worker policy.

### R4. Overload and readiness recovery are demonstrated locally

Exercise bounded pool saturation and release through existing local database
infrastructure. Record acquisition outcomes and the readiness transition under
the current refresher policy, then verify useful work and readiness recover
when the load/held connections are released. Distinguish pool saturation while
the server answers from a silent connection fault. Recovery must not require a
process restart or increasing capacity merely to conceal retained slots.

Retain the shared-pool readiness probe, current thresholds/cadence, acquire and
session budgets, request admission, and worker capacity validation. A local
scenario demonstrates recovery, not stability of a production fleet. Document
the evidence that would reopen capacity/readiness decisions: sustained waits,
database pressure and correlated readiness loss under a representative workload.
An actual defect discovered by the accepted diagnostic remains in scope for
the smallest upstream reconsideration; measurements alone do not authorize a
deployment or an unbounded tuning exercise.

Nearest falsifier: normal work can resume after saturation but readiness remains
stuck, or readiness returns without successful ordinary database work after the
silence scenario. Use existing policy bounds to interpret recovery timing.

## Compatibility, exclusions and proof boundary

Retain PostgreSQL 14+, existing supported PgBouncer transaction-mode contracts,
session-budget verification, password refresh and shutdown budgets. Do not
require PostgreSQL 17's `transaction_timeout`, change migration history or schema,
or add a new retry/queue policy. Library alternatives remain evidence-driven
technical choices; no wholesale driver migration is required by this task.
Optional profiles remain inert or
absent as selected. A configuration or signal compatibility change must be
explicit in Technical Design, with its consumers identified.

Implementation chooses cases and commands in the existing harness. The local
acceptance boundary is the repository's matching build and relevant tests plus
the accepted bounded cancellation, acquisition and overload/recovery observations
above. Observed database behavior follows [PostgreSQL
validation](../../docs/validation/postgres.md); reuse existing relay/protocol
fixtures in [postgres.rs](../../test/tests/postgres.rs) and existing adequate
proof. No new cloud environment, benchmark service, exhaustive profile/server
matrix or full-repository validation is required. Do not claim optional or
unexecuted observations passed.

## Technical Design inputs and reopen conditions

Resume Technical Design from the [reopen result](design-transition.md) and
[alternatives comparison](research/alternatives-reopen.md), replacing the
unreviewed application-facade draft. The current selected direction keeps native
SQLx and temporarily backports the five-second whole-return timeout into the
published sqlx-core 0.9.0 source; it is not a stable release upgrade. Technical
Design owns exact source/custody, Cargo and image delivery, optional-profile
containment, retirement, observation placement and proof feasibility. Evaluate
total maintenance cost, not patch line count alone. Supported library options
come first; any unavoidable local dependency delta has one explicit owner.

Reuse the bounded probe only for its recorded mechanism and environment; it
does not establish full five-second backport, adapter, image or CI acceptance.
Account for healthy reuse, silence during successful return, pending-BEGIN
safety and acquisition cancellation. Preserve finality and bounded eventual
recovery. These are technical decisions, not user library-selection questions.

Reopen Specification for changed supported behavior or failure/finality meaning;
Intake only for changed requester outcome or authority. If an existing driver
limitation makes a requirement infeasible without materially expanding scope,
return the concrete limitation and smallest alternative to the responsible
phase owner. Current default capacity and readiness have no open decision.
