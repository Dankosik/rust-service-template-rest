# Durable JetStream messaging

<!-- template:begin messaging:docs-durable-messaging -->
`MESSAGING=nats-jetstream` retains an optional NATS JetStream capability. It is
inert by default (`MESSAGING=none`), needs no PostgreSQL or jobs selection, and
reuses `jobs-worker` when it needs consumption. Selecting it does not create a
business event, an HTTP operation, a stream, or an additional release binary.
The worker refuses locally when no typed handler is registered rather than
silently consuming data. `OUTBOX=postgres` is a separately selected
PostgreSQL/jobs extension; it is unavailable in a messaging-only selection.

## Event contract and Go interoperability

Features create an immutable typed event once, outside a retryable transaction:
logical ID, stable type, positive schema version, nonzero UTC occurrence time,
and JSON payload. Composition maps the registered `(type, version)` to a fixed
subject and registers typed handlers. Domain code never receives subjects,
consumer names, broker metadata, retries, or ACKs.

The adapter uses the Go wire unchanged: `Message-Id`, `Event-Type`,
`Event-Schema` (`vN`), `Created-At`, and `Nats-Msg-Id`; the original publication
ID is the logical ID. It preserves payload bytes and identity across attempts.
It accepts Go RFC3339 timestamps with offsets/fractions and emits UTC
RFC3339Nano-compatible text. Boundary admission enforces the Go identity,
header (8 KiB), route, schema, and payload limits before a handler allocates or
runs. Unknown handler/schema, subject mismatch, and invalid typed JSON are
permanent. Fixture provenance and CI use actual Go encoding and decoding in
both directions; a hand-written equivalent encoder is not compatibility proof.

## Delivery and settlement

Publication succeeds only after a positive JetStream ACK for the expected
stream, including a duplicate ACK. Invalid input, pre-dispatch cancellation,
or a definite broker refusal is rejected. A lost response, post-dispatch
cancellation, or other inconclusive result is ambiguous: it is neither success
nor rejection and must be retried with the same immutable ID.

Delivery is at least once and has no ordering guarantee. The adopter must make
the handler's effect durably idempotent by logical ID for the entire retention,
DLQ, restore, and replay horizon; in-memory state and broker deduplication do
not establish that property. A successful handler is followed by confirmed ACK;
a lost ACK can redeliver an effect already completed.

Handlers have a 30-second limit. Retryable failures and timeouts use delayed
NAK at 1s, 5s, 30s, and 2m; the fifth failure transfers to DLQ, and deliveries
beyond it bypass the handler. Panic stops the worker unready with the source
unacknowledged. Shutdown cancels unfinished work for redelivery. Consumer
ownership is restricted to the named durable consumer: explicit ACK,
`DeliverAll`, `AckWait=41s`, unlimited broker delivery, `ReplayInstant`, fixed
filter, and `MaxAckPending` equal to configured concurrency. The application
never creates, deletes, or repairs streams or a durable cursor.

## DLQ, restore, and bounds

Malformed, permanent, and exhausted messages publish to the distinct DLQ before
the source ACK. The transfer retains the raw payload and Go identity/trace
headers, adds `Original-Subject` and the closed dead-letter reason, and uses
Go's deterministic `dlq-` identity. An ambiguous DLQ publish or source ACK
requests a 30-second source redelivery; a definite refusal or failed redelivery
instruction leaves the source unacknowledged and faults the worker. An oversized
retained source record or DLQ envelope stops unacknowledged without truncation,
ACK, or handler execution. Restore is an explicit helper, not an endpoint or
automation: it validates the original event and derives Go's deterministic
`redrive-` ID, so repeated restoration stays deduplicable.

The worker reserves capacity before pull and bounds retained raw delivery bytes
by `concurrency * (payload limit + 8192) <= 64 MiB`. Network operations consume
at most 5 seconds and no more than their caller's remaining deadline. Operators
own streams, retention, capacity, replicas, discard policy, and broker
deduplication windows; application health reads cached topology only.

## Configure, operate, and remove

The API may retain an inactive producer when no messaging URL is configured.
Consumer mode needs complete source, consumer, and DLQ configuration. Startup
requires NATS JetStream >= 2.12.3, validates named topology and bounded limits,
and fails with sanitized configuration, authentication, connection, topology,
bounds, closed, or timeout reasons. Readiness uses the existing refresher; a
lost connection fails its next evaluation. On shutdown, readiness drains, pulls
stop, admitted handlers settle under the existing shared deadline, application
tasks join, and dependency close waits for the NATS closed event. A forced drain
is degraded, never a clean completion.

Production topology requires R3 replicas across independent failure zones and
`sync_interval: always`. R1 is only for local development and tests. Choosing a
different sync interval requires a named operator's explicit acceptance of the risk
that acknowledged data can be lost; startup cannot certify those deployment
properties. Adapter telemetry has only closed publication, handler, DLQ, and
connection result vocabularies. It never labels metrics or logs with payloads,
credentials, arbitrary errors, or event IDs.

To remove the profile, initialize or migrate a service with `MESSAGING=none` so
the initializer removes its code, configuration, tests, images, CI, and this
guide together. Do not remove a profile from a live service by deleting only a
binary or configuration section.
<!-- template:end messaging:docs-durable-messaging -->

<!-- template:begin outbox:docs-durable-messaging-outbox -->
## Transactional publication

The outbox reuses the prepared event's exact bytes, route, and identity. A
broker ACK before fenced jobs completion can still lead to a duplicate publish
after a crash or unknown completion result, so consumer durable-effect
deduplication by logical ID remains authoritative beyond broker and jobs
dedupe horizons. The outbox guide owns the caller-transaction, live-key,
outage, recovery, and capacity rules. See [PostgreSQL transactional
outbox](postgres-transactional-outbox.md).
<!-- template:end outbox:docs-durable-messaging-outbox -->
