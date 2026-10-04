# Technical design: bounded PostgreSQL checkout and HTTP attempt

> Integration disposition: the cleanup mechanism and its immediate/one-second
> timing below are historical and superseded by the accepted library-owned
> five-second return policy in [Persistence Architecture](../../../docs/architecture/persistence.md#query-pool-checkout-and-cancellation).
> `Checkout`, application detach/hidden-return ownership and `with_connection`
> are removed. The HTTP cutoff minus 100 ms, exhausted-entry refusal,
> same-identity recovery, transaction finality and pending-BEGIN safety remain.
> Foreground HTTP budget arithmetic excludes asynchronous native cleanup.


Status: ready

Authority: [ready Specification](../spec.md), SHA256
`c2eb7713b7f37768df06c1c04775633a1ae465cc028e170c612fb80515e5668e`.
Baseline: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
This artifact owns mechanism and the numerical policy; Implementation owns
concrete regression cases and execution. No dependency upgrade or schema change.

## Selected mechanism

Keep SQLx 0.9.0 and the existing plain `PgPool`. Centralize the template's
query-pool checkouts in one private cancellation guard in
`crates/infra-postgres/src/checkout.rs`. Export one callback boundary,
`with_connection`, for current non-transaction callers; `in_tx_with` uses
the same guard internally.

The guard owns an `Option<PoolConnection<Postgres>>`. While operation work
borrows its connection, dropping the owner synchronously detaches and drops
the raw connection. This releases the pool permit without a network wait,
never returns uncertain state to the pool, and requires no cleanup task,
queue, tracker, new pool type, or executor implementation. The current
cancelled-BEGIN safety guarantee therefore lasts for the entire checkout,
rather than ending when BEGIN succeeds. Removing the narrower
`DiscardOnDrop` is replacement by this stronger guard, not reliance on the
unreleased upstream fix.

After ordinary completion, explicitly return the connection through SQLx's
public `PoolConnection::return_to_pool()` future under a one-second timeout.
That future takes ownership of the live connection and decrement-size guard
before polling. Cancellation or timeout drops both, closing the socket and
releasing capacity. The timeout includes after-release buffer shrinking,
rollback flush, return ping, and any close path selected by SQLx. Capture the
absolute cleanup deadline when operation work ends; do not restart it.
The current pool has `min_connections = 0`; changing that reopens this
source-sensitive reasoning.

One second reuses the adapter's existing idle-ping budget: both operations
need to establish that this physical connection can answer a protocol
round trip. Cleanup may include queued rollback, so a healthy but slower
connection is deliberately discarded instead of extending the bound.
This is a policy allocation, not a measured latency claim. It is below R1's
three-second maximum; cancellation during work or pending BEGIN reclaims
immediately, and cancellation during release drops the owning return future.

Release is maintenance: failure or expiry never replaces an already computed
operation result, acknowledged COMMIT, or authoritative driver error. It may
emit a bounded cleanup diagnostic; it does not report rollback confirmation.
Ordinary successful release preserves connection reuse. Failed BEGIN and
CommitUnknown discard immediately; a completed callback error or known
commit rejection may return only after its transaction has dropped and queued
rollback, through the same bounded return future.

`with_connection` lends only `&mut PgConnection` for its callback. It does
not lend the pool guard or transaction-control authority. Preserve acquire
failure versus callback result without inventing a common error type: its
outer `Result<T, sqlx::Error>` is acquisition, and `T` is the callback's
output (including the caller's own `Result`). The internal guard is not
exported. This lets jobs retain their existing acquire/query classification.

### Why this mechanism

Resolved SQLx 0.9.0 source and official
[pool connection source](https://docs.rs/sqlx-core/0.9.0/src/sqlx_core/pool/connection.rs.html)
show that `return_to_pool` owns the permit, including while awaiting
`raw.ping()`. PostgreSQL `ping` always writes a new Sync and waits for its
answer (`sqlx-postgres/src/connection/mod.rs:180-187`).

- An `after_release` timeout is insufficient: a second, unbounded driver
  ping follows the hook. The same applies to bounded idle `before_acquire`.
- `close_on_drop` holds the permit during its five-second close; it misses
  R1 and cannot preserve ordinary reuse as an always-on policy.
- Public `detach` is sufficient for cancellation, but detaching every
  success would violate the reuse requirement.
- A driver fork, vendored dependency, pool replacement, custom executor, or
  background return manager adds more lifecycle and compatibility work than
  taking ownership of the existing return future. No surviving requirement
  needs those mechanisms.

`return_to_pool` is public but `#[doc(hidden)]`: it is not advertised as
a stable extension contract. Accept that narrow, explicitly version-sensitive
use against the locked 0.9.0 source, isolated in one file. Recheck ownership,
drop, and permit release before a SQLx upgrade; replace this shim when a
selected released driver provides bounded return with equal cancellation
safety. No other driver internals or private fields are used.

## Canonical caller scope and bypasses

This is a guarantee for template-owned query access through `in_tx[_with]`
and `with_connection`, not a type-enforced guarantee for arbitrary SQLx
APIs on the retained raw `PgPool`. Direct `Pool::acquire`, `Pool::begin`,
or query execution against `&PgPool` can bypass it. Current production
query-pool callers must all move to the canonical boundaries in this PR:

| Current caller | Target |
| --- | --- |
| `infra-postgres/transaction.rs` | private guarded checkout for BEGIN through COMMIT/release |
| `infra-postgres/pool.rs::verify_session` | `with_connection` for settings readback |
| `infra-postgres/probe.rs` | `with_connection` for readiness ping |
| `migrate/src/lib.rs::verify_history` | `with_connection` for startup history inspection |
| `infra-idempotency-store/maintenance.rs` | `with_connection` for the direct writer check; existing transactions keep `in_tx_with` |
| `infra-jobs/claim.rs::send_claim` | `with_connection` inside existing backstop |
| `infra-jobs/attempt.rs::write_batch, send_outcome` | same, retaining operation error classification and fences |
| `infra-jobs/maintenance.rs` | same for direct sampling/admission access; existing transactions keep their own limits |

Document those boundaries and extend the existing disallowed-method policy
for direct pool acquisition outside its owner, with narrow intentional test
exceptions. This lint does not detect implicit pool execution: final source
inventory/review must check that bypass separately. Raw database fixtures may
remain raw when they deliberately exercise SQLx or hold a lock; they do not
establish the template's cleanup guarantee.

SQLx's initial `PgPoolOptions::connect_with` releases its freshly acquired
internal connection directly (`pool/options.rs:537-557`), without the
problematic return ping. The dedicated migrator remains a direct session.
Jobs' dedicated LISTEN reconnection pool is owned by `PgListener`, outside
the shared query pool and its permits; this design makes no bounded-listener
teardown claim. Its existing listener/worker lifecycle remains its owner.
A requirement to bound arbitrary driver-owned pools reopens Specification.

## HTTP budget and failure policy

Use the existing `infra_http::RequestDeadline` constructed by the hardened
chain immediately before its tower timeout. The absolute instant already
includes body reads, identity capture and handler work; never derive a fresh
eight-second budget at database entry. No task-local context, new transport
crate dependency, database configuration key, or global timeout reduction.

Reserve **100 ms** at the end of the configured request for terminal response
mapping. This is a conservative allocation of the smallest complete request
budget the existing HTTP configuration admits (`HttpConfig::validate`), for
a much smaller operation: the current boundaries only select/construct a
bounded in-memory response and return through the hardened chain; they do
no subsequent database or provider I/O. Idempotency captures its bounded
response body inside the timed attempt; webhook success has no body.
The reserve is an engineering policy, not a 100 ms delivery SLA or a claim
about slow clients, scheduler starvation, or streamed response completion.
Reopen it if these terminal paths acquire asynchronous I/O, unbounded body
work, or measured evidence shows this allowance inadequate.

The two currently supported HTTP PostgreSQL operations are:
`infra-http/idempotency/execute.rs::Idempotency::execute` and
`infra-http/webhooks.rs::receive`. Each computes
`attempt_end = RequestDeadline.at() - 100 ms` and bounds its complete store
or receipt future with `timeout_at(attempt_end, ...)`. Include acquisition,
BEGIN, all statements/user closure/body capture, COMMIT and bounded release.
Check that the cutoff is still in the future **before polling** the operation;
an exhausted budget returns the existing unavailable response without entering
the database. There is no invented minimum useful-query latency: positive
remaining time permits one best-effort attempt; its timer bounds even an
arbitrary sequence. No BEGIN or COMMIT is promised after the cutoff, and no
attempt starts when zero useful time remains.

For defaults, after the full acquire ceiling has been spent:

`acquire <= 3 s; remaining complete attempt <= 4.9 s; response reserve = 0.1 s; total <= 8 s`.

The 4.9 seconds includes transaction work, COMMIT and up to one second of
release; it is not a per-statement allowance or an independent fresh timeout.
Earlier acquisition or body handling changes the remaining amount, never the
absolute cutoff. A request configured at 100 ms, or reaching database entry
with 100 ms or less left, receives unavailable without a database attempt;
document that admitted configuration consequence rather than silently
raising the configured outer deadline. Cancellation by a client or the outer
deadline also drops the same store/receipt future and reaches the guard.

The inner cutoff maps through current transport failures:
`IdempotencyUnavailable` with existing retry hint for idempotency and
`ServiceUnavailable` for webhook receipt. The outer tower deadline retains
its canonical `gateway_timeout` 504. Do not add a new code, expose SQL errors,
or wait out the reserve merely to force an outer 504. Keep one outcome metric
per request and the existing abandonment semantics for actual outer
cancellation. Both 503 responses mean ownership/outcome was unavailable, not
proof that the effect failed. Cancellation while COMMIT is in flight, or
after acknowledgement while response/release is pending, still requires
the same idempotency key or webhook message identity. There is no retry
inside this mechanism.

Session `statement_timeout = 8 s` and idle-in-transaction timeout remain the
non-HTTP server fallback for jobs/autocommit and a lost client. They do not
establish a transaction-duration bound, and a local HTTP cutoff is not proof
of server rollback. Keeping them avoids changing valid jobs, PgBouncer
startup/server-budget deployments, or migration budgets. The caller remains
responsible for meaningful transaction boundaries, external effects and
future request paths; those paths must adopt the existing deadline explicitly.

## Transaction truth and material flows

1. Acquire failure returns its current classification; no work or effect
   began. Cancelling acquisition is the driver's existing acquire path,
   which drops its floating connection/permit; no checked-out guard escapes.
2. Pending BEGIN cancellation drops the guarded connection even before
   SQLx increments transaction depth. No return ping can recycle an open
   transaction. BEGIN error is preserved and the connection discarded.
3. Work error preserves the caller error. Drop the SQLx transaction before
   bounded release so queued rollback is included. Cancellation at any await
   drops the checkout instead; neither path claims confirmed rollback.
4. Successful writable work keeps existing `Tx::not_aborted` proof and
   conditional SELECT 1. `connection(tx)` withdraws proof; a failed check
   remains `CommitFailed` and prevents COMMIT. Savepoint recovery, read-only
   exemption and isolation rendering are unchanged.
5. COMMIT keeps the existing SQLSTATE split: known rejection is
   `CommitFailed`, unresolved reply is `CommitUnknown`. Successful COMMIT
   remains success even when return cleanup fails. Outer cancellation may
   prevent delivery but is never relabelled as known non-commit.
6. Jobs/backstops and startup/readiness timers cancel their own operations
   through the same guard. They acquire no HTTP reserve. Database/pooler
   disconnect detection may lag after detach; only local capacity is bounded.

## Ownership and implementation footprint

Placement follows existing owners; no crate/dependency boundary changes and
no separate ownership fork survive. The new file exists solely because all
query-pool owners need the same cancellation/return lifecycle. Its deletion
would duplicate that rule or reopen R1.

| Responsibility | Exact file actions, visibility and allowed role | Proof/reopen owner |
| --- | --- | --- |
| Guard and bounded return | Add `crates/infra-postgres/src/checkout.rs`: private guard, `pub with_connection`, `pub(crate)` checkout access for transaction owner; export helper/module from `lib.rs`. Existing SQLx/Tokio only; no business/HTTP policy | adapter tests and database suite; reopen this design for changed driver return semantics |
| Transaction finality | Modify `transaction.rs` to use guard and preserve all existing Tx/TxError surfaces | database transaction proof; Specification if finality changes |
| Canonical pool access | Modify the eight source files in the caller table, only their checkout scopes; no changes to jobs semantics, migrator execution, schema or query ownership | affected callers' existing proof and final bypass inventory |
| HTTP reserve/cutoff | `harden.rs` keeps deadline source and owns a small shared crate-private cutoff calculation/100 ms policy under existing request-budget template markers; `idempotency/execute.rs` and `webhooks.rs` consume it and own failure mapping | existing HTTP/store/router proof; HTTP owner for added callers or changed post-attempt work |
| Regression correction | `test/tests/postgres.rs`: named borrowed-connection case uses `connection(tx)`; R1-R3 regression additions stay with existing owner/harness selected by Implementation | real database suite for observed claims |
| Access policy/docs | `clippy.toml` narrow direct-acquire policy; `docs/architecture/persistence.md` removes old query-macro/observed deferrals and revises return/budget decisions; `docs/http-idempotency.md` and existing inbound webhook guide update early-unavailable/same-identity behavior where needed | docs-check, matching lint surface, final review |
| Profile compatibility | Existing template markers contain HTTP deadline changes. `scripts/lib/template_init.py` must select `request-budget` for inbound webhooks too; update matching expectations in `scripts/tests/template-init-safety.py` and `scripts/tests/template-sync-canary.py`. This is the existing marker carrier, not a new profile or dependency | existing template validation route; reopen only if a new runtime dependency is required |

No query text needs to change for the mechanism. Existing query macros and
root `.sqlx/` remain canonical; if concrete implementation changes a checked
statement it must regenerate metadata through the existing owner. No source,
test, manifest, lockfile, CI or generated file has been edited in this phase.

## Proof boundary and movement

Implementation selects the smallest existing-harness cases distinguishing
cancelled BEGIN/work/COMMIT, cleanup that never resolves, and a failed return
after acknowledged success. Required claims: local permit reclamation within
one second once cleanup starts (immediate discard during work cancellation),
no reuse of unresolved state, normal reuse, preserved commit/error identity,
and actual borrowed-connection aborted-transaction coverage. Controlled proof
and source inspection do not claim a production blackhole observation.

HTTP proof covers configured absolute cutoff after earlier work/acquisition,
early unavailable without dispatch when exhausted, complete multi-statement
attempt cancellation, canonical responses and same-identity recovery. Preserve
existing direct/PgBouncer startup and server modes in the existing database
validation route; do not create a new harness or matrix. Ordinary matching
build/tests, docs consistency, final assembled independent review and PR CI
remain the delivery owner. This phase ran no build/database/performance test.

Independent Technical Design Review permits movement. Reopen Specification for
changed outcomes or expanded driver-owned session scope; reopen this design
for unbounded return ownership, new query-pool bypass, reserve evidence, or a
selected SQLx upgrade. Implementation test selection is not an upstream gap.

## Technical design review

Independent Technical Design Review: PASS, no surviving findings. Reviewer
`/root/postgres_design/design_review` (Astra high, fresh read-only context)
checked fixed candidate SHA256
`512c6c6dca22eff2288b8f95fb597d8289ecc0c2677ed8c3e42f400cbf0ac090`
against the ready Specification above. Subsequent edits only mark readiness,
record this receipt, and resolve the already-owned initializer carrier to its
exact current files; selected mechanism and review scope are unchanged.

Falsifiers checked: cancellation retaining the permit; a current canonical
query-pool bypass; fresh HTTP budgets or lost commit finality; missing
RequestDeadline in inbound-only generated services; cleanup replacing the
operation result. None survives the design. The reviewer checked current
callers, initializer, SQLx 0.9.0 source and persistence/validation owners.
This establishes design readiness, not working code or runtime behavior.

`make docs-check` passed on 2026-10-02: 847 total, 370 unique, zero errors.
No implementation or runtime tests ran in this phase. Its sole descendant
returned the review above and completed.

## Phase transition

```text
status: ready
owner: TechnicalDesign
result: specs/infra-postgres-closeout/design/technical-design.md
review: specs/infra-postgres-closeout/design/technical-design.md#technical-design-review
movement_evidence: R1-R4 have one selected mechanism, current caller scope, numerical policy, file owners, proof boundaries and independent PASS; no downstream mechanism or requester decision remains
reopen_owner: none
next_owner: Planning
```
