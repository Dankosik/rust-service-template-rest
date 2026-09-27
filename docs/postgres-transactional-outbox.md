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

The stored job payload is format version `1`: fixed subject, logical and
publication identity, type, schema, occurrence time, and standard padded
base64 for the exact prepared JSON bytes. Those fields are immutable intent.
Base64 preserves the original byte spelling through JSONB; optional trace
context stays in the jobs trace carrier and is not part of equality. The
existing serialized-jobs limit remains in force: `PreparedEvent::enqueue`
returns `OutboxEnqueueError::PayloadTooLarge` with the smaller effective
immutable-intent allowance and never truncates an event.

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
releases the pending/running unique-key slot; terminal retention later removes
the historical row. Consumer effects need their own durable logical-ID
deduplication for every stream-retention, DLQ, restore, and replay horizon.

The canonical guides show the transaction boundary explicitly:

```rust,ignore
#[derive(Debug, thiserror::Error)]
enum CreateError {
    #[error(transparent)]
    Event(#[from] domain_events::EventError),
    #[error(transparent)]
    Messaging(#[from] infra_messaging::MessagingError),
    #[error(transparent)]
    Outbox(#[from] infra_messaging::outbox::OutboxEnqueueError),
    #[error(transparent)]
    Tx(#[from] infra_postgres::TxError),
    #[error("outbox live identity changed; retry the transaction")]
    RetryTransaction,
}

let event = domain_events::Event::new(logical_id, occurred_at, payload)?;
let prepared = registry.prepare(&event, max_payload_bytes)?;
infra_postgres::in_tx(pool, async |tx| {
    write_business_record(infra_postgres::connection(tx)).await?;
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
and five seconds. Unavailable or ambiguous publication, and recoverable broker
topology or configuration refusal, returns a 30-second snooze. Snooze refunds
the attempt, including the final one, so a long broker outage retains pending
intent instead of exhausting it. Malformed stored intent is a visible terminal
job failure, never a completed publication.

Operators retain the pending job when recovering or rolling back an application
change. Do not disable the outbox while unpublished intent is live, and do not
delete unfamiliar pending publication jobs as rollback cleanup. Restore the
compatible outbox owner or roll forward so it can publish the retained identity.
No migration, second outbox table, or schema cleanup is required for this
design.

## Capacity, lifecycle, and proof

`jobs-worker` runs a separate one-slot publication engine beside ordinary jobs.
With ordinary jobs and outbox active, the shared PostgreSQL pool requires
`jobs.max_workers + 5` connections: the ordinary `N + 2` allowance, one
publisher, and two management connections. An outbox-only worker requires
three. Separate registrations and claim loops ensure that occupied webhook
slots do not prevent due publication; this makes no throughput or latency SLO.

The two engines, NATS consumer work, and dependency resources share the
process's existing absolute drain, cleanup, and close deadlines. They do not
receive a full grace period each. At a forced drain, existing jobs release and
fencing rules retain recoverable publication intent.

The assembled validation plan must use real PostgreSQL and NATS to cover
commit/rollback, same/different/lost live-key outcomes, final-attempt snooze,
outage recovery, uncertain ACK/completion, durable consumer effects, and
publication while webhook slots are occupied. It also retains meaningful
outbox-only, combined, and neighboring-profile representatives plus locked
offline metadata. These are required final-validation and CI obligations; this
guide does not claim they have run for any candidate.
<!-- template:end outbox:docs-postgres-transactional-outbox -->
