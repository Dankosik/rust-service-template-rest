# Specification: infra-postgres closeout

Status: ready; cleanup timing superseded by the accepted PostgreSQL pool
resilience integration. R2 HTTP cutoff and R3 transaction truth remain active.
[Persistence Architecture](../../docs/architecture/persistence.md#query-pool-checkout-and-cancellation)
owns the replacement policy.

Authority: [Intent](intent.md). Baseline:
`67be869acea112af271ec8ba621cbc50ae9d36b7`. This contract defines behavior;
Technical Design selects its mechanism and concrete budget allocation.

## Outcome and necessary changes

One separate PR closes the adapter's cancellation recovery defect, makes the
default budget relationship defensible, repairs the missing regression path,
and removes contradictory guidance. No requirement below authorizes deployment
or changes the user's requested delivery boundary.

### R1. Cancellation cannot indefinitely consume pool capacity

For a connection checked out from the shared query pool created by
`infra_postgres::connect`, including all its current canonical callers and
transaction paths, cancellation during BEGIN, transaction work, or COMMIT must
not leave its pool permit dependent on an unbounded driver return ping or TCP
failure detection. The same applies to rollback/return cleanup after a failed
operation. A silent network peer may prevent successful database work, but must
not permanently strand the local slot after its owner stops waiting.

This guarantee covers query-pool consumers, including jobs and maintenance;
it does not claim bounded reclamation for every raw SQLx pool. The jobs
worker's dedicated LISTEN session uses its own private pool, and the migrator
uses a dedicated connection outside the query pool. Those two session
lifecycles retain their existing owners and are outside R1's reclamation claim.

SQLx owns a five-second whole-return cleanup bound. This replaces the earlier
requirement that reclamation fit inside the three-second acquire budget; an
acquisition during cleanup may time out, while later work recovers. The bound
starts with the native return operation after cancellation or completion;
it concerns release of local capacity, not confirmation of server rollback or
availability while the database remains unreachable. It assumes the Tokio
runtime can make progress. A connection whose clean reusable state cannot be
established in that bound must not be reused. Successful ordinary operations
retain connection reuse; the repair does not impose disconnect-after-every-query
as the default policy.

The SQLx 0.9.0 cancelled-BEGIN guard remains necessary until a released and
selected driver fix or an equally strong replacement proves that path safe.
An upstream merge alone does not satisfy that condition.

### R2. Request and database budgets compose honestly

The configured HTTP request timeout remains the outer deadline. PostgreSQL
policy for the existing supported HTTP paths must reserve time for the
terminal response after acquisition, database work, and commit handling.
The policy must state what it bounds: a statement limit is not a transaction
limit, and lowering a statement limit alone must not be documented as bounding
an arbitrary sequence of statements. Request cancellation must reach the
database operation's ownership boundary; R1 bounds the subsequent local cleanup.

Technical Design must close the numerical allocation and enforcement scope,
including where the remaining request budget originates, what happens when
there is insufficient time for useful work/commit/response, and what is still
caller-owned for multi-statement transactions. Defaults must satisfy the stated
inequality with a positive terminal-response reserve; arbitrary positive
reserve values chosen only to make arithmetic pass are insufficient. The
allocation must be justified from existing timeout owners and supported paths.
This is no promise that an arbitrary future sequence of SQL or external effects
will finish, and does not require a universal deadline framework. Select the
smallest change that closes the current mismatch and states its actual limits.

HTTP failure responses continue through the existing canonical transport
failure mapping. A caller ceasing to wait during COMMIT does not establish that
the effect failed. The repair must preserve the same-operation reconciliation
path and must not add a blind retry.

Background jobs, autocommit maintenance operations, and the dedicated migrator
retain their own lifetime/budget authorities. An HTTP deadline must not become
a global session assumption that silently truncates valid non-HTTP work.
Supported direct PostgreSQL and PgBouncer startup/server-budget modes remain
supported. Any changed admitted default or operator setup must be documented
with its compatibility consequence in the same PR.

### R3. Existing transaction truth remains protected

The writable `in_tx` boundary cannot report committed success after a swallowed
SQL failure left the transaction aborted, including when the statement used
`connection(tx)` rather than the `Tx` executor. A genuine savepoint recovery
and a subsequent successful statement remain valid. The existing read-only
exception, isolation selection, and caller-owned transaction boundaries remain.

Retain the distinction between known non-commit (`CommitFailed`, including a
failed check before COMMIT) and unresolved commit (`CommitUnknown`). Preserve
SQLSTATE identity and same-operation idempotency reconciliation. No generic
retry loop or rollback-success claim is added. Cancellation must never produce
a false committed-success result; reclaiming a socket does not resolve whether
a COMMIT already took effect on the server.

The regression named
`a_failure_on_the_borrowed_connection_is_found_before_the_commit` must actually
borrow via `connection(tx)` and demonstrate its stated safety behavior, not
duplicate the direct-executor scenario.

### R4. Documentation reflects the selected implementation

Persistence guidance must consistently describe existing checked `query!`
statements, root `.sqlx/` metadata, and `observed` statement instrumentation.
Remove the stale HTTP-idempotency/jobs deferrals. Update cancellation cleanup
and budget explanations to the selected behavior, with source-backed versus
observed evidence distinguished. Retain migrations outside application startup.

## Review finding dispositions

| Finding | Disposition |
| --- | --- |
| SQLx return ping can retain a pool permit without a deadline after transaction cancellation | Repair under R1; baseline driver source establishes the risk, not a runtime reproduction. |
| Default 8 s HTTP, 3 s acquire, 8 s statement budgets have no composed reserve | Repair under R2; numeric policy and enforcement belong to Technical Design, not a speculative global timeout reduction. |
| Borrowed-connection regression uses the normal executor | Repair under R3 using the named production seam. |
| Old query-macro/tracing deferrals contradict current source and newer documentation | Repair under R4. |
| Readiness shares the pool and uses consecutive-failure threshold | Deliberately unchanged documented overload/reachability tradeoff; reopening needs evidence that the repair invalidates it. |
| Custom Tx/proof, observed, and buffer shrinking may cost performance | No performance rewrite or improvement claim without measurement; unrelated benchmarking is a non-goal. |
| DSN admission and TLS modes | Deliberately unchanged deterministic policy; `require` is not represented as `verify-full`, and deployment-independent mandatory TLS is a non-goal. |
| SQLx versus another driver | Retain selected SQLx 0.9.0; existing capability and upstream source evidence do not justify a dependency migration. |

## Evidence and completion boundary

Current factual owners are [Persistence Architecture](../../docs/architecture/persistence.md),
[`pool.rs`](../../crates/infra-postgres/src/pool.rs),
[`transaction.rs`](../../crates/infra-postgres/src/transaction.rs),
[`postgres.rs`](../../test/tests/postgres.rs), and
[`http.rs`](../../crates/config/src/http.rs). The parent review inspected
SQLx 0.9.0 `sqlx-core/src/pool/connection.rs:275-331`: `return_to_pool` awaits
`raw.ping()` while holding capacity without a timeout. The adapter's idle
`before_acquire` ping deadline does not bound that path. Upstream
[cancelled-BEGIN fix](https://github.com/transact-rs/sqlx/pull/4394) is merged but
was unreleased when checked on 2026-10-02; recheck release/adoption before
removing the existing guard.

Implementation chooses the smallest meaningful regression proof for R1-R3
and validates changed documentation. Claims of observed database behavior use
the existing real-database validation owner and harness; a controlled unit
failure establishes only its controlled boundary. Apply the repository's
ordinary matching build/tests, final independent review for the assembled
data-integrity/concurrency change, and applicable CI gates for the requested
PR. Do not introduce a new test stack, paid experiment, exhaustive environment
matrix, or performance acceptance gate solely for confidence. Unrun optional
runtime observations are disclosed without being reported as passes.

## Next owner and reopen conditions

Technical Design owns bounded cleanup, the request-budget allocation and
propagation mechanism, affected-consumer compatibility, and file ownership.
Specification reopens if that design changes these observable outcomes or
cannot retain supported consumers. Intake reopens only if requester meaning or
external-effect authority changes. Definition completion does not claim an
implemented repair or fulfill the requested PR by itself.

## Definition review

Independent Specification Review: PASS, no surviving findings. The fixed
semantic candidate was `spec.md` SHA256
`0f3de06a16659e1a998203d72ff0713fa069f5fec16ba4c5ad135335ed53cd81`
and `intent.md` SHA256
`7a3c17dad362a24fe5ab3991b97a0dea4cd5db945ac7de357728aaa772918db2`;
the initial subsequent changes only marked readiness and recorded this receipt. Reviewer
`/root/postgres_definition/spec_review` checked the contract, owners, relevant
source/test/docs, and SQLx 0.9.0 source read-only. No build or runtime claim.
In particular, SQLx's old cancelled-BEGIN close path can retain capacity for
five seconds; retaining it unchanged does not satisfy R1. Technical Design
must preserve its safety guarantee while meeting the reclamation bound.

Definition clarification for Technical Design: R1 names the existing query-pool
owner explicitly. Dedicated LISTEN/migration sessions were not part of the
reviewed query-pool guarantee; no canonical shared-query-pool caller is excluded.
This preserves the accepted outcome and independent-session lifetime boundary,
so the earlier verdict remains applicable to that unchanged semantic scope.
