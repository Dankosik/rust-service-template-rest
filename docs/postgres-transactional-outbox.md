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
A crash or an unknown completion result after ACK may publish again with the
same identity, which consumers must tolerate. Existing `complete_in_tx` and
unknown-commit rules still apply to handlers that combine a durable effect with
completion: do not replay the business closure after an unknown commit.

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

After backup restore, invalidate saved pre-restore recovery tokens, commands,
and receipts. Restore queue/history/sequence consistently and handlers compatible
with outstanding intent, reconcile possible prior effects, and re-inspect the
restored identities before recovery. The jobs history/index migrations and the
[all-old-retention-owners-stopped gate](background-jobs.md#upgrade-and-custody)
apply to publisher jobs too. Old-binary rollback can delete retained failures.

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
<!-- template:begin jobs-reference:docs-outbox-reading-reference -->

The [reading-counter reference](../test/README.md#reading-counter-recovery-reference)
stores the immutable operation's event identity and occurrence time at acceptance.
Its consumer commits a separate permanent effect marker and article aggregate in
an independent database before ACK. It exercises lost ACK, transport cleanup and
same-identity replay, producer backup/restore, and actual feature execution before
and after a template update. Broker deduplication windows and queue retention do
not determine the recipe's business replay lifetime.
<!-- template:end jobs-reference:docs-outbox-reading-reference -->
<!-- template:end outbox:docs-postgres-transactional-outbox -->
