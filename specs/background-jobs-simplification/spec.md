# Background jobs simplification

Status: ready (independent Definition review: PASS)

Authority: [intent](intent.md). Evidence:
[definition baseline](research/definition-baseline.md). Current unchanged
contracts are in [the adopter guide](../../docs/background-jobs.md) and
[async architecture](../../docs/architecture/async.md). This contract replaces
their conflicting lease, unknown-outcome, storage and telemetry decisions;
implementation must update those active owners rather than append contradictions.

## Durable attempts and uncertainty

1. A claimed kind has a finite timeout under the current policy bounds. Its
   one database lease expires at claim statement time plus that timeout plus
   **60 seconds** of safety margin. There is no periodic claim extension, upkeep
   task, or worker/queue heartbeat substitute. This deliberately trades faster
   crash rescue for fewer writes. Recovery can take the whole remaining lease
   plus normal polling/database latency; no failover-survival guarantee exists.
   Preserve the conservative monotonic deadline derived before sending the claim
   and a two-second local cancellation margin. Dispatch, handler timeout and
   persistence must fit within it; delayed acknowledgement cannot extend it.

2. Only an acknowledged committed claim admits a handler. Unknown-commit claims
   admit nothing, perform no attribution read, and recover by lease expiry if
   they committed. That committed claim still spends an attempt. A normal claim
   keeps sequence-generated `(id, claim_generation)` fencing, database time,
   current attempt bounds and unknown-kind nonconsumption. Expiry makes a row
   reclaimable; a newer generation, not expiry alone, invalidates a fenced write.

3. After a handler result, retry only the same intended fenced outcome operation
   and its fixed arguments within the remaining local deadline. Do not rerun the
   handler to resolve an acknowledgement loss. Repeated outcome operations may
   affect at most one transition: the running-state predicate and generation
   guard make later repeats no-ops. A zero-row result is no longer attributed to
   a particular earlier transition. When acknowledgement remains unknown, report
   uncertainty and allow normal expiry recovery. Do not retain reconciliation
   queries or registry state merely to prove metric attribution. This is scoped
   to repeatable queue transitions, not arbitrary business transactions.

4. On stop, disable readiness and stop claiming, then drain. At the drain
   boundary, a handler result already available takes precedence over forced
   cancellation. Preserve its intended success/failure/snooze and persist it
   within the remaining bounded cleanup budget; never turn it into a refund and
   immediate rerun. Only unfinished handlers are cancelled and eligible for
   fenced release, refunding one attempt once. Unpersisted known outcomes remain
   recoverable by expiry. Cleanup exhaustion remains observable and cannot
   extend the parent grace deadline. Shutdown must not race a release against
   completion for the same locally known result.

5. Attempt counters and duration describe worker-observed execution, including
   a known handler result even if its queue write is unacknowledged. Operation
   failures/unknown persistence remain distinguishable. Queue snapshots describe
   sampled database state. Document that neither attempts nor shutdown counters
   are an exact durable-transition ledger; remove proven-outcome attribution.

## Atomic effects and explicit delay

6. Add `complete_in_tx` for an adapter's caller-owned PostgreSQL transaction.
   Business writes and current-generation completion commit together or neither
   does. It uses the supplied transaction connection, with no second connection
   or independent commit. A stale/missing/non-running claim is a typed failure;
   the supported usage must cause all that transaction's business effects to
   roll back. An acknowledgement loss remains unknown: the engine must not
   blindly replay the business closure, and cannot undo a committed completion.
   The handler contract explains the required error propagation. Exactly-once
   effects outside that transaction are not promised; retain stable effect IDs
   and provider idempotency guidance. Ordinary handlers remain supported.

7. Add explicit requested retry timing (`retry_after`) and snooze. Retry-after
   is a failure with caller-selected delay instead of exponential delay; it
   spends the attempt and exhausts normally at the kind's cap. Snooze means
   intentional deferral (for example rate limiting): it refunds the attempt,
   retains job identity/live uniqueness, and does not report failure or increase
   later exponential backoff. Both schedule from database time, preserve fencing,
   and do not become claimable before that time. Zero is allowed and obeys normal
   polling. Use the existing enqueue delay domain (whole microseconds, at most
   36,500 days); invalid duration is a typed rejection, never a panic or a
   silently clamped schedule. Repeated unknown-commit snoozes cannot refund twice.
   Snooze is deliberately not bounded by the failure attempt cap; document that
   adapters must use it only for intentional deferral.

## Storage, validation and propagation

8. Add a forward migration to `payload jsonb` and nullable `unique_key text`;
   leave the merged migration unchanged. All queue users require a UTF8
   database, established before admitting jobs work. New enqueue rejects NUL
   in JSON string values and object keys in Rust before issuing SQL, including
   escaped representations; a literal backslash followed by `u0000` is valid.
   Retain pre-SQL size/unique-key/delay checks and usable caller transactions on
   validation rejection. Unique keys retain exact equality and byte limits,
   independent of locale/collation. JSON values, not serialized formatting or
   object order, are the payload contract; JSONB last-key-wins normalization for
   duplicate object keys is explicit. No new payload indexing is required.

9. Existing compatible rows keep their IDs, semantic payloads, unique keys,
   state, scheduling, attempts and generations. Incompatible historical bytes
   (including NUL-bearing JSON), invalid JSON, or incompatible server encoding
   cause the entire migration to fail without changing rows or migration history.
   Never substitute payloads, drop rows or silently mark them failed. Document
   a preflight and bounded stop-producers/stop-workers migration sequence; old
   bytea binaries and new JSONB binaries are not a supported mixed deployment.
   Recovery from a refused migration leaves the old schema/data intact; operator
   repair of incompatible data is a separately authorized business action.
   After successful conversion, recovery uses forward-compatible code, not an
   old binary or destructive down migration. No live rollout is part of this PR.

10. Capture and extract trace context with the installed OpenTelemetry TextMap
    propagator, carrying `traceparent` and `tracestate`. Keep linked producer to
    consumer semantics, bounded storage and no baggage propagation. Missing or
    malformed context creates an unlinked attempt without failing the job.
    Preserve valid legacy traceparent-only rows. Remove the handwritten codec.

## Bounded work and reuse

11. Oldest-ready age uses indexed per-registered-kind lookup over due pending
    jobs; future jobs and terminal history do not contribute. Queue counts may
    become capped lower bounds rather than exact counts: expose/document the
    cap and whether a sample is censored. Design chooses capped work per worker
    or one coordinated observer, but no repeated unbounded live-row aggregation
    on every worker. Expose last successful observation/freshness; failed samples
    must not publish old values as current or an empty queue. Before the first
    success, distinguish no observation from zero backlog. Keep label cardinality
    bounded and explicitly disposition the existing unregistered aggregate.

12. Claim locking must scale with the selected batch rather than registered
    kinds multiplied by available slots. Preserve disjoint claims under
    concurrent workers, indexed access, unknown-kind isolation, and earliest-due
    preference (`not_before`, then ID) without promising strict execution order.
    Document that a sustained backlog in one kind may delay another; per-kind
    quotas/priorities or a fairness scheduler are out of scope.

13. Replace handwritten splitmix64 with an existing supported random source;
    preserve ordinary retry distribution bounds `n^4 ±10%`. Validate static kind
    names with a reusable const assertion while preserving typed runtime
    validation for reachable invalid input. Consolidate redundant `e6` enqueue
    cases only after mapping their distinct committed/rolled-back/in-flight
    holder, isolation and transaction-usability contracts; retain every distinct
    behavioral oracle, not an arbitrary test count.

14. Roadmap 10.5 webhook dispatch and 10.6 outbox delivery reuse `infra-jobs`
    for durable scheduling/attempt infrastructure; provider/business semantics
    remain with their adapters. Technical Design decides whether actual shared
    service/jobs-worker lifecycle responsibilities justify extraction now.
    A possible third binary is context, not sole justification for a new crate.
    No future product, new queue framework or generic workflow engine is added.

## Proof boundary and unchanged behavior

Retain enqueue `ON CONFLICT DO NOTHING`, `Created`/`Duplicate`, caller isolation
and transaction fate, 25 maximum attempts, ordinary exponential retry behavior,
terminal retention (completed 24 hours, failed 7 days), and optional profile
inertness. No upstream HTTP-idempotency or PostgreSQL commit-classifier redesign.

Implementation chooses minimal tests under the repository validation owner.
Reuse real-PostgreSQL coverage for fencing/reclaim, disjoint claims, enqueue,
transactional completion commit/rollback/stale ownership, repeated uncertain
outcomes and snooze, and both successful/refused legacy migration. Controlled
runtime proof covers no-heartbeat timeout behavior, drain-result races, bounded
cleanup, trace state and stale observations. Query cost/locking claims require
representative indexed plan or concurrency evidence, not wall-clock promises.
Required CI-owned database/profile/image gates stay CI-owned; no duplicate
mandatory local infrastructure matrix is added. Final integrated independent
review must cover the concurrency, transactional and migration changes.

Technical Design closes mechanisms, API ownership, exact sampling/censoring
policy, rollout sequence and lifecycle extraction. Reopen Definition if a
required invariant cannot hold, compatibility needs lossy conversion, or a
new user-visible policy is needed. No requester-owned question remains.
