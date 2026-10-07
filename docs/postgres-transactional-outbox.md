# PostgreSQL transactional outbox

<!-- template:begin outbox:docs-postgres-transactional-outbox -->
`OUTBOX=postgres` retains durable publication intent for a service that also
selects PostgreSQL, jobs, and JetStream messaging. It is not a runtime mode or
a second queue: the initializer rejects an incomplete selection, and removing
the outbox profile removes this guide with its code, configuration closure,
tests, and CI routing.

## Transactional intent

Create the typed event once, before a retryable business transaction. Its
prepared form enqueues the private `publish_domain_event` job through the
canonical jobs API using the caller's `&mut infra_postgres::Tx`. The adapter
does not commit, open another connection, issue queue SQL, or replay the
business closure. A rollback exposes neither the business write nor the intent;
a commit exposes both.

Event preparation serializes the payload once, retaining at most the supplied
`max_payload_bytes` of encoded JSON. Buffer capacity requests grow only up to
that ceiling; small payloads do not eagerly reserve the whole allowance. Excess
output is counted and discarded until serialization finishes, so a late
serialization failure still wins over the size refusal. Subject validation
precedes serialization; a successfully serialized oversized payload is rejected
before identity/time validation. Preparation refusal has no database or broker
effect. This bounds retained serialized output, not allocator rounding or the
CPU and allocations inside an application-defined serializer.

Successful preparation keeps the exact compact JSON bytes in shared `Bytes`
backing. Cloning a prepared event shares that payload allocation and copies its
owned metadata. The backing remains live until its last owner is dropped;
caller-retained events, clones, and concurrent preparation remain caller-owned.

The stored job payload is format version `1`: fixed subject, logical and
publication identity, type, schema, occurrence time, and standard padded
base64 for the exact prepared JSON bytes. Those fields are immutable intent.
Base64 preserves the original byte spelling through JSONB; optional trace
context stays in the jobs trace carrier and is not part of equality. The
existing serialized-jobs limit remains in force: `PreparedEvent::enqueue`
returns `OutboxEnqueueError::PayloadTooLarge` with the smaller effective
immutable-intent allowance and never truncates an event.
The jobs ceiling is 262144 encoded JSON bytes, including immutable metadata
and the padded base64 text. For an event with `O` bytes of serialized metadata
(including the empty base64 field), the raw prepared-payload allowance is
`3 * floor((262144 - O) / 4)`, with zero available space when metadata consumes
the ceiling. `PreparedEvent::outbox_payload_limit` reports that event-specific
allowance. Base64 encoding owns additional storage while the intent is built;
the event preparation ceiling is not a bound on all simultaneous representations.

The unique key is `event-` plus the lowercase SHA-256 of the logical-ID bytes.
It fits the jobs key bound while accepting the Go-compatible 256-byte logical
ID. A collision still compares the complete immutable intent and cannot merge
different events.

On an ordinary enqueue duplicate, jobs locks and compares the live pending or
running row in the same caller transaction. `Same` accepts the already-live
intent; `Different` is an event-ID conflict; `NoLongerLive` means the row
ended between the duplicate and comparison and requires the caller's existing
transaction-retry path with the same prepared event. It is neither acceptance
nor conflict. Live-key uniqueness is finite: the fenced terminal transition
releases the pending/running unique-key slot; completed retention later removes
the historical row, while failed rows remain until explicit recovery or discard. Consumer effects need their own durable logical-ID
deduplication for every stream-retention, DLQ, restore, and replay horizon.

The canonical guides show the transaction boundary explicitly:

```rust,ignore
#[derive(Debug, thiserror::Error)]
enum CreateError {
    #[error(transparent)]
    Messaging(#[from] infra_messaging::MessagingError),
    #[error(transparent)]
    Outbox(#[from] infra_messaging::outbox::OutboxEnqueueError),
    #[error(transparent)]
    Tx(#[from] infra_postgres::TxError),
    #[error("outbox live identity changed; retry the transaction")]
    RetryTransaction,
}

let event = domain_events::Event { id: logical_id, occurred_at, payload };
let prepared = registry.prepare(&event, max_payload_bytes)?;
infra_postgres::in_tx(pool, async |tx| {
    write_business_record(tx).await?;
    match prepared.enqueue(tx).await {
        Ok(infra_messaging::outbox::OutboxEnqueued::Created
            | infra_messaging::outbox::OutboxEnqueued::Duplicate) => Ok(()),
        Err(infra_messaging::outbox::OutboxEnqueueError::LiveIdentityChanged) => {
            Err(CreateError::RetryTransaction)
        }
        Err(error) => Err(error.into()),
    }
}).await?;
```

The caller's transaction/retry owner maps `RetryTransaction` to its bounded
retry policy. Its error type retains `From<infra_postgres::TxError>`: a
`TxError::CommitUnknown` reconciles the business operation's identity and never
replays this closure.

The publication handler reuses the stored route, identity, and bytes. Positive
JetStream ACK is required before the jobs supervisor performs fenced completion.
The messaging adapter admits ACK-capable File/default persistence for the source
and, for a consumer, its DLQ. Operators still own sync, replicas, retention and
storage recovery. Jobs fencing protects queue transitions, not a publish already
sent by an expired attempt.
A crash or an unknown completion result after ACK may publish again with the
same identity, which consumers must tolerate. Existing `complete_in_tx` and
unknown-commit rules still apply to handlers that combine a durable effect with
completion: do not replay the business closure after an unknown commit.

## Executable durable-effect example

The opt-in [example](../test/examples/messaging_recovery.rs) publishes a typed
counter increment and consumes it through the actual `jobs_worker::run` entry.
Its [shared effect](../test/examples/messaging_recovery/effect.rs) is also the
real-PostgreSQL test owner. Select `OUTBOX=postgres` to retain the example;
removing outbox removes its executable, SQL, proof and deferred factory API.
This does not add a shipped business kind or a default migration.

Use only an owned example database. Apply the template migrations, then apply
[the example schema](../test/fixtures/messaging_recovery.sql) explicitly. Create
source and DLQ streams as described in [durable messaging](durable-messaging.md).
The default route is `recovery.counter.incremented`; configure the worker's
existing PostgreSQL and messaging settings, durable name, filter and DLQ subject.
The worker also registers the existing test-only `test.probe` ordinary job
(`integration_tests::jobs::Probe`) through its canonical jobs registration.
The SQL includes its `probe_attempts` table. Enqueue it through `infra_jobs::enqueue`
with `ProbeAction::Succeed` or a bounded `ProbeAction::Sleep { millis }` for a
capacity observation. With one ordinary slot, configure at least six pool
connections under the existing `jobs.max_workers + 5` policy.
`RECOVERY_SUBJECT` selects the consumer route when a fixture needs isolation.

```bash
# DATABASE_URL selects the owned producer database; retain every argument for reconciliation.
cargo run --locked -p integration-tests --example messaging_recovery -- \
  produce increment-001 2026-10-06T12:00:00.123456789Z orders 7

# APP__POSTGRES__DSN selects the worker database; the worker owns one admitted pool.
cargo run --locked -p integration-tests --example messaging_recovery -- \
  worker --config env/config/local.toml
```

`produce` optionally accepts the subject after the delta. It prepares the event
once and commits its example producer counter, logical-ID receipt and immutable
outbox intent in one transaction. A retry first arbitrates the same logical-ID
receipt: equal metadata and payload digest return `producer_receipt_reconciled`
without another counter change or enqueue; changed meaning refuses. The receipt
stores a digest, not replay bytes, and cannot recreate a deleted publication job.
`intent_committed` confirms only that transaction. An unknown
producer COMMIT is reported as unresolved; keep the event's exact logical ID,
time and payload for reconciliation. The command never retries a transaction.

The `publisher` mode registers the ordinary probe and uses the existing outbox
publisher without a consumer handler. `consumer` registers the same accepted
effect handler against its own worker-owned pool; that profile still has its
existing outbox engine, with no producer intents in the effect database. The
original `worker` mode shares ordinary jobs, publication and consumption in one
process/pool. These example modes let the native recovery fixture restore the
producer and effect databases independently; they add no production role knob.

The payload contains only `counter_id` and signed 64-bit `delta`; unknown JSON
fields are rejected. The receipt key is `(consumer_scope, logical_id)`. Receipt
meaning compares event type, schema version, UTC occurrence time including
nanoseconds, counter and delta. JSON spacing/order, publication ID and trace
context do not change that meaning. A changed meaning is a permanent conflict.
Receipts have no automatic expiry.

Each delivery makes one explicit READ COMMITTED transaction attempt. First,
`INSERT ... ON CONFLICT DO NOTHING RETURNING` arbitrates the receipt key. A
successful insert and the counter mutation commit together. A competing insert
waits for the original transaction; a fresh statement then checks the committed
receipt. Equality is duplicate success. Conflict is permanent failure; a
missing receipt, failed statement, timeout, cancellation or unknown COMMIT
remains unresolved and uses the existing retryable disposition. A failed
counter mutation rolls back its new receipt too.

After `CommitUnknown`, a later delivery uses the same key and the same INSERT
arbitration. It can establish an equivalent committed effect or proceed once
an earlier attempt is known not to have committed. A racing plain SELECT that
finds no receipt cannot establish absence. No business closure is replayed
automatically, and external provider effects have no atomicity claim here.

`Registration::with_postgres_messages` declares consumer intent during ordinary
registration, so missing consumer configuration refuses before dependency I/O.
Declare routes in `Registration.messages`, then install one factory accepting
`PgPool` and that same mutable registry. After pool admission and migration
history verification, the worker invokes it once before broker admission.
It may only perform bounded synchronous composition; it must not open a pool,
perform provider I/O, spawn tasks or retry. Duplicate factories/handlers, empty
handlers, errors and panics refuse startup through the retained-resource cleanup
owner. Consumer, ordinary jobs and publisher share the existing pool ceiling;
the factory gains no separate readiness, close or lifecycle owner.

The authored `messaging_recovery` integration target covers durable duplicates,
meaning conflicts, rollback, competing commit/rollback and lost COMMIT replies.
The worker process suite covers factory error/panic/empty-handler cleanup after
pool admission. These are proof surfaces, not a claim that a particular candidate
has passed them. Run them in the assembled delivery's database/broker plan.

## Outage and recovery

The reserved publisher has 25 maximum attempts and a 30-second handler budget.
Each broker operation is bounded by the lesser of the caller's remaining time
and five seconds. Any failed publication, rejected or ambiguous, is a retryable
jobs failure: it spends an attempt and waits for the jobs backoff (`attempt^4`
seconds with independent +/-10% jitter). With immediate failures and no extra
floor, queueing, downtime, or handler cost, the nominal delays before attempt 25
sum to `1,763,020 seconds` (about 20.4 days), with a +/-10% jitter-only range.
This is not a delivery bound: handler time, retry floors, outage, backpressure,
and scheduling can lengthen the horizon. Retrying an ambiguous publication keeps the unchanged publication ID, which the broker
deduplicates only inside the stream's duplicate window (two minutes by
default); the fourth retry already waits longer. A retry outside the window
stores the event again, which consumers absorb by deduplicating on the logical
ID. After the last attempt the job stays visible in the `failed`
state until explicit recovery or discard. Malformed stored intent is a visible terminal
job failure, never a completed publication. Keep handlers compatible with
outstanding kind and stored payload versions throughout rolling deployment and
restore. A kind rename is not migration, and redrive does not repair poison
intent; restore compatible code or use a separately reviewed data conversion.

Operators retain the pending job when recovering or rolling back an application
change. Do not disable the outbox while unpublished intent is live, and do not
delete unfamiliar pending publication jobs as rollback cleanup. Restore the
compatible outbox owner or roll forward so it can publish the retained identity.
Use the jobs worker's [inspection and recovery commands](background-jobs.md#inspect-and-recover-retained-jobs).
Include `publish_domain_event` in the explicit fleet handled-kind union. Inspect
the failed id/kind/version and reconcile a possible prior publication before
redrive. Redrive preserves the same job ID, logical/publication IDs, route,
unique key, and exact prepared bytes; it resets the retry budget on that row
without re-enqueuing or replaying the business operation. A competing live
identity refuses recovery as a conflict. An unknown outcome requires fresh
inspection, never automatic replay. Discard permanently abandons unpublished
intent and removes its history.

Broker deduplication still ends at the configured duplicate window. Consumer
logical-ID deduplication must cover the full retained-failure, redrive, restore,
stream-retention and DLQ horizon; the default two-minute broker window is not
that guarantee. Failed custody and permitted manual replay have no automatic
expiry, so no finite consumer deduplication TTL covers every permitted replay.
Retain durable logical-ID effect identity for the full permitted replay lifetime,
or reconcile effects and explicitly constrain replay before expiring that
identity. No exactly-once effect is promised.

The handler's logical-ID receipt and business mutation belong in the same
database transaction. Insert the receipt under a unique constraint, apply the
business mutation only for the first insertion, and commit both before
returning success. A failed mutation rolls back the receipt too; recording a
receipt before a separate effect can suppress needed recovery. On an uncertain
commit, reconcile or re-enter that same durable identity arbitration rather
than minting a new ID. For an external effect, use the provider's idempotency
and reconciliation contract; a local receipt alone cannot make it atomic.
The joint messaging proof uses separate receipt and business tables through
`in_tx`, covering both repeated publication and lost settlement after commit.

After backup restore, follow [restore and reconcile](#restore-and-reconcile).
The jobs history/index migrations and the
[all-old-retention-owners-stopped gate](background-jobs.md#upgrade-and-custody)
apply to publisher jobs too. Old-binary rollback can delete retained failures.

## Restore and reconcile

An outbox completion means the broker returned a PubAck. It does not mean a
handler committed an effect, and a failed job does not establish that the
broker never accepted an earlier ambiguous publication. Completed publication
jobs remain for 24 hours; failed jobs remain until explicit recovery/discard.
The queue is not a permanent event archive and does not automatically republish
completed jobs when broker storage is lost.

PostgreSQL atomicity preserves business writes and intent together within one
database. It does not certify PostgreSQL fsync, `synchronous_commit`, failover
or backup durability. Establish those with the database operator, alongside
the broker's independent storage contract. A PG backup, source/DLQ snapshots,
consumer positions and external effect history have distinct recovery points.

| Restored combination | Risk to reconcile |
| --- | --- |
| Broker older than producer PG | An event can be absent while its job is completed or already removed by 24-hour retention. Recover from retained event history or another accepted reconstruction source, not an automatic retry of the business operation. |
| Effect database older than broker consumer state | The consumer can have ACKed an effect the restored database no longer contains. Reconcile logical IDs and deliberately choose replay positions before resuming. |
| Broker or queue older than committed effects | Retained source/intent can replay an already committed effect. Restore durable effect identities with the effects and retain their replay protection. |
| Lost broker data and expired publication history | Recovery is outside the outbox guarantee. An accepted RPO that requires this replay needs service-owned archival or reconstructible event history; a longer dedup window cannot restore missing bytes. |

Before production, the service owns RPO/RTO, the permitted replay lifetime,
backup cadence, off-site custody, the reconciliation authority and the evidence
that closes recovery. The template supplies no business values for them.
Rehearse the procedure against the accepted recovery target, preserving exact
event identities and bytes; matching row/message counts alone is insufficient.

1. Pause affected producer writes, publication claims, consumer pulls and
   retention owners before changing their recovery points. Keep the failed
   publication custody and compatible handlers intact.
2. Restore producer business/outbox data and queue history/generation sequence
   consistently. Restore effect data and its logical-ID receipts together.
   Restore source, DLQ and consumer state with native broker tooling.
3. Invalidate saved pre-restore recovery tokens, commands and receipts. Read
   actual stream ranges, consumer positions, queued/failed identities and
   committed effects to identify missing publications and possible duplicates.
4. Reconcile each affected logical ID against its canonical effect owner.
   Choose deliberate replay/redrive positions for retained records. Use
   PostgreSQL-only jobs `inspect`/`failed`/`unhandled`/`redrive` for job custody;
   these commands do not operate the broker DLQ. Unknown outcomes require fresh
   inspection rather than replay of a saved command.
5. Resume compatible owners and verify useful business progress, queue age,
   source/DLQ backlog and settlement. Record recovered identities and remaining
   loss against the accepted RPO/RTO before declaring the recovery complete.

The separate one-slot publisher bounds concurrency, not accepted queue depth.
Size the PG backlog for broker outages, including base64/JSONB and retained
failure history. Queue-depth and failed-job metrics are per-process capped
samples (1000 means at least 1000); their registered-kind counts and
oldest-available age are operational signals rather than proof of all pending,
delayed, unhandled or already published work. Use the existing bounded operator
inspection with the fleet's explicit handled-kind union for retained custody.
Do not sum identical per-process samples as independent queue populations.

## Capacity, lifecycle, and proof

`jobs-worker` runs a separate one-slot publication engine beside ordinary jobs.
With ordinary jobs and outbox active, the shared PostgreSQL pool requires
`jobs.max_workers + 5` connections: the ordinary `N + 2` allowance, one
publisher, and two management connections. An outbox-only worker requires
three. The publisher engine is built beside the ordinary one: both share the
worker's one `LISTEN` connection outside the pool, one retention loop, one
sampler over both registered-kind sets, and one worker id. The publisher slot remains held through outcome bookkeeping and
its retry waits, separately from ordinary capacity. Separate registrations and
claim loops ensure that occupied webhook slots do not prevent due publication; this makes no throughput or latency SLO.

The two engines, NATS consumer work, and dependency resources share the
process's existing absolute drain, cleanup, and close deadlines. They do not
receive a full grace period each. NATS admission failure can prevent ordinary
jobs from starting, and a critical engine/consumer failure stops the whole
process. The one publisher slot and this shared failure domain remain deliberate
limits; independent throughput or availability needs reopen the architecture.
At a forced drain, existing jobs release and fencing rules retain recoverable publication intent.

The assembled validation plan must use real PostgreSQL and NATS to cover
commit/rollback, same/different/lost live-key outcomes, final-attempt failure,
outage recovery, uncertain ACK/completion, durable consumer effects, and
publication while webhook slots are occupied. It also retains meaningful
outbox-only, combined, and neighboring-profile representatives plus locked
offline metadata. These are required final-validation and CI obligations; this
guide does not claim they have run for any candidate.
<!-- template:end outbox:docs-postgres-transactional-outbox -->
