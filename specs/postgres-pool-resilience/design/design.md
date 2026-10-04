# PostgreSQL pool resilience design

Status: ready. Independently reviewed Technical Design. Authority:
[Intent](../intent.md) SHA256
`f8a0f671878ddeeae9a01c2af9cad7859cf3d7d5b3f3a73e81404add0eecfab4`;
[Specification](../spec.md) SHA256
`0411915db5fbe82fcde601ca6f1621248046cfc3b491f3151983c59e606ff480`.
Base: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
[Dependency custody](dependency-custody.md) and [ownership map](ownership.md)
are part of this candidate. The earlier application-facade design is replaced.

## Decision and evidence

Keep native SQLx pooling. Backport a five-second timeout around its **whole
owned return operation**, in the published sqlx-core 0.9.0 dependency. Pool
capacity, connection return, recycling, internal tasks and shutdown stay in
SQLx. Application code neither calls the driver's hidden return method nor
introduces a new Pool, pooled-connection wrapper, Executor, cleanup task or
retry loop. Public PgPool and SQLx query/transaction/migration types remain.

The [source and causal probe](../research/sqlx-release.md) establish why a
callback is insufficient: SQLx runs another unbounded ping afterward, and
closed-pool/lifetime branches bypass the callback. The
[alternative comparison](../research/alternatives-reopen.md) covers released
Deadpool, mobc-sqlx, deadpool-postgres, bb8-postgres and infrastructure pooling.
Generic Deadpool can retain SQLx, but would move rotation, exact idle retirement
and close accounting into the template. Whole-driver replacement also ports
query checking and transaction/migration contracts. The chosen temporary
backport preserves those existing owners at the cost of source/delivery
custody; [that custody](dependency-custody.md) explicitly includes the 110-file,
approximately 649 kB vendored package and its removal, not just the eleven-line
production delta. No released SQLx fix is claimed.

## Library return, cancellation and finality

The source delta adds one five-second return timeout around the existing
`Floating::return_to_pool()` future in `PoolConnection::return_to_pool`,
matching the upstream-aligned ownership boundary. It covers after_release,
final ping, graceful close on pool closure and the maximum-lifetime branch.
On expiry, dropping the owned future drops the raw connection and then the
capacity guard; the pool may open a replacement without waiting for a response
from the abandoned peer. Native minimum-connection maintenance remains after
that result. No driver connection is detached from accounting and no maximum
is increased. Existing buffer shrinking, idle checks and ordinary reuse stay.

PostgreSQL ping queues Sync, flushes pending output and drains ReadyForQuery
responses; it is a real round trip. SQLx return therefore still flushes queued
rollback before successful reuse. The timeout bounds that existing library
work rather than adding another ping. Healthy returns keep their connections.
A silence fault after successful SQL is covered by the same whole-return bound.

The existing DiscardOnDrop protects unresolved BEGIN via close_on_drop. Keep
it armed until BEGIN acknowledges and preserve its existing five-second bound.
SQLx 0.9.0 increments transaction depth only after the acknowledgement, so
removing this guard could recycle an open transaction; the separate newer
upstream BEGIN fix is deliberately not included in this narrow backport.

Cancellation during later work, borrowed savepoints, pre-commit verification
or COMMIT still drops the existing SQLx transaction and return ownership.
Preserve Tx.not_aborted, pre-commit SELECT 1 and classify_commit unchanged.
Successful cleanup or a discarded socket provides no evidence that COMMIT
succeeded, failed, or can safely be retried. CommitUnknown and the existing
caller cancellation/deadline path remain authoritative. There are no SQL or
transaction retries.

Native acquisition already owns a permit/floating connection under its total
acquire timeout. Cancellation while waiting, connecting or idle-checking drops
that ownership. Do not add an acquisition task or shield the acquire future
from caller cancellation. Ordinary acquisition stays three seconds; cleanup
is five seconds, so an acquire during cleanup can time out. After capacity is
reclaimed and connectivity permits new sessions, later work recovers without
restart or capacity growth. No first-request success SLA is created.

A released local slot does not prove that the server immediately noticed a
closed or unreachable client. Physical PostgreSQL sessions can linger while
replacements connect. Server/pooler limits, cleanup and operational headroom
own that transient; the application pool maximum is not a bound on that
instantaneous physical session count.

## Ordinary acquisition observation

Add one ordinary acquisition entry point at the existing
`infra-postgres::observe` owner: `acquire(pool: &PgPool, operation: &'static str)`,
returning the native `PoolConnection<Postgres>` and unmodified sqlx::Error.
A private shared observer times a supplied native acquisition future; this
entry point passes `pool.acquire()` and the constructor passes its native
connect_with future. It does not own connection cleanup, impose
another timeout, count attempts, parse SQL or alter error identity. Reexport
this function from the crate for existing provider callers. This is an
ordinary acquisition entry point, not a replacement pool API/type.

The operation argument is a callsite-owned static label. Its values are the
finite existing operation summaries or the literals in the table below; no
request, SQL parameter, user or job identity is passed. The code-owned slow
threshold is one second, located beside the current pool budgets. Use Tokio
Instant for this new elapsed observation. The event budget is read from the
native pool options so a test/custom native pool's configured budget is
reported truthfully.

| Event | Level | Trigger and fields |
| --- | --- | --- |
| `postgres_pool_acquire_slow` | warn | Native acquire returns Ok after more than 1 s; pool="postgres", operation, elapsed_seconds, threshold_seconds=1 |
| `postgres_pool_acquire_timeout` | warn | Native acquire returns PoolTimedOut; pool="postgres", operation, elapsed_seconds, budget_seconds |

Fast success has no new event. PoolClosed and execution errors are never
reported as acquisition timeout. An unfinished cancelled acquire emits neither
success nor timeout. Fields contain no Dsn, connection options, raw error text,
SQL, parameters or request-derived labels. Existing service/instance and span
context remains supplied by telemetry. The logical PostgreSQL pool name is
consistent with current pool metrics; no process-local pool-id registry is added.

Configure native acquire_time_level and acquire_slow_level Off in the adapter
constructor, because the helper owns these same named-path diagnostics.
Native SQLx logging remains useful outside this path, but it cannot alone
supply timeout outcomes and operation context. Do not emit two new acquisition
attempt records for one acquire. Keep the existing transaction wait histogram
at Observed::waited, with the same name, labels, buckets and failed-acquire
meaning; it remains transaction-only. Keep statement and transaction duration
boundaries unchanged while adding the helper.

### Covered callers and exact placement

| Current caller | Observation placement / static operation |
| --- | --- |
| pool.rs::connect | Keep native connect_with and run that future through the shared acquisition observer as "connect", before session verification. With the retained min_connections=0, its network work is one native acquire followed by synchronous release; this preserves the current initial-connection path and introduces no extra ping or lazy factory. |
| pool.rs::verify_session | Within the existing observed("check session budgets", ...) future, explicitly acquire as "check session budgets" and run the unchanged checked query on the borrowed native connection. |
| transaction.rs::in_tx_with | Replace the single raw acquire with the helper as "transaction"; keep Observed::waited and DiscardOnDrop ordering. in_tx, jobs maintenance, idempotency arbitration/cleanup and webhook receipt work inherit this path. |
| probe.rs::PostgresProbe::check | Acquire as "readiness"; preserve ping and ProbeError mapping under the existing refresher deadline. |
| migrate/src/lib.rs::verify_history | Acquire as "check migration history" inside the existing five-second history bound. Dedicated migration sessions remain outside pool acquisition. |
| infra-jobs/src/claim.rs::send_claim | Acquire as "claim jobs", preserving OperationError::Acquire and the existing backstop. |
| infra-jobs/src/attempt.rs::write_batch | Acquire as "complete jobs batch", preserving the existing completion-batch deadline/error owner. |
| infra-jobs/src/attempt.rs::send_outcome | Acquire as "record job outcome", preserving current transition/finality policy. |
| infra-jobs/src/maintenance.rs::check_startup | Acquire as "check jobs startup", with existing writable-session validation. delete_batch and sample_once already enter in_tx_with and need no duplicate observer. |
| infra-idempotency-store/src/maintenance.rs::check_startup | Within its existing observed future, explicitly acquire as "check idempotency startup" and execute the unchanged direct query on that connection. Its retention path already uses in_tx_with. |

The two implicit direct-pool statement acquisitions found in current runtime
owners are session verification and idempotency startup; moving acquisition
inside their existing observed futures preserves those operation durations.
Queries already using Tx or an explicitly acquired connection do not acquire
again and need no wrapper. Inbound webhooks remain covered through in_tx;
there is no reason to change their source just to repeat diagnostics.

The private jobs LISTEN pool is a separate driver transport with existing
listen/reconnect diagnostics; it is not the shared adapter execution pool.
It keeps its current SQLx ownership. Native administrative pools injected by
sqlx::test likewise remain test infrastructure. Document this actual coverage;
arbitrary future external raw-PgPool callers are not universally intercepted.
All native pools using the patched SQLx core receive the return fix, regardless
of whether they use the helper for observation.

## Runtime and delivery boundaries

No composition or process lifecycle boundary moves. The service/worker still
construct the shared pool through infra-postgres::connect, admit migrations,
register shared-pool readiness and existing gauge/password tasks, and close
under the current five-second dependency stage. Native pool.close still wakes
waiters immediately. A close that spends its deadline remains Closed::TimedOut;
this design adds no shutdown allowance or success claim. The native library's
bounded return tasks are library-owned, not new application background tasks.

Retain PostgreSQL 14+, supported PgBouncer startup/server-budget modes, the
direct migrator, prepared statements, password sources and refresh cadence,
three-second acquire, eight-second session budgets, default capacity four,
range 1..500, thirty-minute lifetime and ten-minute idle timeout. The schema,
query strings, offline metadata format, toolchain and SQLx CLI version do not
change. The source backport is the only dependency code change.

[Dependency custody](dependency-custody.md) fixes archive verification, the
source-only lock projection, workspace exclusion, Cargo-chef real-source copy,
optional-profile removal, existing gate selection and upstream retirement.
No raw registry cache edits, unlocked Cargo commands, fork publishing, git
PR-head pin or new CI job is part of the chosen path.

## Connection-budget and operating guidance

Update the existing Persistence guide beside Budgets/Readiness and replace its
superseded warning that cancellation of active I/O retains capacity indefinitely.
State acquisition-event coverage and transaction-only histogram scope. Explain
that a timeout during the five-second cleanup interval can be expected and
that increasing capacity is not the repair for retained slots.

For direct PostgreSQL, calculate configured session allocation from peak
simultaneously live replicas, including rolling replacement overlap:

```text
sum(peak_service_replicas * service_pool_max)
+ sum(peak_worker_replicas * worker_pool_max)
+ simultaneous_LISTEN_sessions + direct_migrators + other_direct_sessions
+ other_applications + administrative_and_reserved_allowance
<= PostgreSQL max_connections
```

Readiness is already inside each shared pool. Each worker process with engines
has one separate LISTEN session, including the shared ordinary/outbox case.
Ordinary workers require pool max >= jobs.max_workers + 2. Preserve current
outbox-specific validation where retained: N + 5 with ordinary jobs, three
outbox-only. A budget below these minima is a conflict, not a valid setting.

Illustrative direct example: server maximum 100 sessions; 15 admin/reserved,
25 other applications, two concurrent migrators and three other direct
sessions. Three services plus one rolling replacement at pool four allocate
16. Two ordinary-jobs workers plus one replacement, each with four workers
and pool six, allocate 18; their LISTEN sessions add three. Total 82, leaving
18 unallocated. These are configured allocations, not average usage or
production-optimal throughput. The transient lingering-backend caveat above
also applies; keep operational reserve rather than claim the formula caps
instantaneous physical sessions after network failure.

For PgBouncer, calculate application client connections against client limits
separately from backend allocations against PostgreSQL sessions. Include
pooler replicas, database/user partitions, normal and reserve pool sizes,
database/user caps, rollout overlap and separate direct/session-mode consumers.
Use actual configured limits. Multiplying application pool maxima does not
by itself count backend sessions.

Size from acquisition waits/timeouts, pool occupancy, request/job latency and
server CPU, I/O, locks and transaction age together. Sustained waits with spare
server capacity may justify testing another pool size; database saturation or
long lock holding can worsen with more concurrent connections. Use a bounded
representative workload, change one value, and inspect recovery. No automatic
tuning or production capacity claim is made.

## Proof and reopen conditions

The earlier design probe demonstrated the owned-return mechanism on local
PostgreSQL 18.6: native retention/PoolTimedOut at 3002 ms, recovery and SELECT 1
at 1035 ms under a one-second candidate bound, same-backend healthy reuse,
silent return after success at 1002 ms, and synchronous unpolled discard.
Its fixture warned of lingering server sessions before outer Compose teardown;
that supports the resource distinction, not a server-rollback claim. It does
not prove the chosen five-second backport, new observations or delivery graph.
No additional design experiment is needed to choose this same owned-future
boundary with the now-reviewed timeout; Implementation supplies acceptance.

Implementation selects concrete cases/commands in the existing harness and
owns final validation. Required behavioral scope remains the Specification:
active silent cancellation, bounded repeated returns, eventual useful work
and healthy reuse; pending-BEGIN/finality oracles; acquisition cancellation
and truthful slow/timeout/closed outcomes across the named paths; responsive
saturation followed by useful-work and readiness recovery. Reuse existing
relay/protocol fixtures and adequate transaction/pooler coverage, without
cloning every case into every caller. The permanent regression must fail on
unpatched SQLx for the retention cause, not a setup error.

The default readiness interval remains 2 s, probe budget 4 s, threshold three;
three-second acquisition fits the probe budget. Sequential checks use Delay
missed ticks. Withdrawal follows the required failed rounds; recovery follows
the next successful completed round. Default stale-after remains
4 + 3 * max(2,4) = 16 s. Record actual timing under this policy, not a fleet SLA.

Local final validation is the matching build and unit tests for the manifest/
several-crate surface, dependency checks, docs consistency and focused real-DB
observations. The existing image, initializer and other CI-selected gates
remain selected with truthful pending status when remote execution is not
authorized. The source-custody proof checks locked metadata, one path-patched
core, equivalent graph/features and retained/absent profile containment.
Do not multiply a full database/profile matrix or claim full-repository proof.

Reopen Technical Design for a graph/source delta beyond the isolated patch,
failed Cargo-chef/profile containment, a changed SQLx return/drop contract,
incomplete observation coverage, or failed cancellation/reuse proof. Reopen
Specification for changed behavior/finality/deployment support or budgets;
Intake only for changed requester meaning/effect authority. Retire the backport
on a suitable released SQLx fix with matching proof. No upstream ETA, release,
remote write, deployment or spending is promised or authorized.
