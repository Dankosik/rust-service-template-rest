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

Features create a typed event once, outside a retryable transaction:
logical ID, stable type, positive schema version, nonzero UTC occurrence time,
and JSON payload. Composition maps the registered `(type, version)` to a fixed
subject and registers typed handlers. Domain code never receives subjects,
consumer names, broker metadata, retries, or ACKs. A payload type implements
`Serialize` to be published and `DeserializeOwned` to be delivered; a service
needs only the direction it uses.

The adapter uses the Go wire unchanged: `Message-Id`, `Event-Type`,
`Event-Schema` (`vN`), `Created-At`, and `Nats-Msg-Id`; the original publication
ID is the logical ID. It preserves payload bytes and identity across attempts.
It parses and emits RFC3339 timestamps with offsets and fractions, the text
Go's `time.RFC3339Nano` produces; spellings only Go's lenient parser accepts
are malformed. Boundary admission enforces the Go identity,
header (8 KiB), route, schema, and payload limits before a handler allocates or
runs. Unknown handler/schema, subject mismatch, and invalid typed JSON are
permanent. Fixture provenance and CI use actual Go encoding and decoding in
both directions; a hand-written equivalent encoder is not compatibility proof.

A handler is registered for one exact `(type, version)`. A delivery with a
version no handler knows is permanent and transfers to the DLQ, so deploy the
consumers of a new schema version before its first producer. A record that
arrived too early is recovered with the restore helper.

The envelope is the Go template's header set, not CloudEvents. The CloudEvents
NATS binding (`ce-id`, `ce-type`, `ce-time`, `ce-source`, `ce-specversion`)
carries the same identity under other names; adopting it is a wire break that
both templates and every deployed stream would take together, and no consumer
outside these services reads the events today. Reopen the choice when events
are offered to a party that does not use this adapter.

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

Handlers have a 30-second limit. Retryable failures, timeouts, and panics use
delayed NAK at 1s, 5s, 30s, and 2m; the fifth failure transfers to DLQ, and
deliveries beyond it bypass the handler. A failure of one delivery never stops
the worker: it is logged, counted, and redelivered by the broker. Shutdown
cancels unfinished work for redelivery. A delivery the client prefetched but
no handler admitted is returned at drain with a one-second redelivery delay
instead of waiting for ack wait; the broker still counts it as a delivery. The
worker declares its named durable
consumer (create or update): explicit ACK, `DeliverAll`, `AckWait=41s`,
unlimited broker delivery, `ReplayInstant`, and the fixed filter. `MaxAckPending`
keeps the broker default, which bounds the durable across all replicas; each
replica bounds its own in-flight work by the configured concurrency. The
application never creates, deletes, or repairs streams. Only a deleted or
replaced durable consumer stops the worker unready. The broker reports that on
a waiting pull; after any other pull-stream error, including two missed
15-second idle heartbeats, the worker asks the broker for its durable and
stops when the broker answers that the durable or its stream no longer exists.
An unanswered lookup is a broker outage, which the worker rides out.

## DLQ, restore, and bounds

Malformed, permanent, and exhausted messages publish to the distinct DLQ before
the source ACK. The transfer retains the raw payload and Go identity/trace
headers, adds `Original-Subject` and the closed dead-letter reason, and uses
Go's deterministic `dlq-` identity. A failed DLQ publication keeps the source
and requests its redelivery in 30 seconds; a lost source ACK or redelivery
request is left to the broker's ack wait. A retained source record larger than
the payload limit is malformed and transfers to DLQ without handler execution.
Restore is an explicit helper, not an endpoint or
automation: it validates the original event and derives Go's deterministic
`redrive-` ID, so repeated restoration stays deduplicable.

The worker pulls through the client's pull stream with batches of
`concurrency` messages and `concurrency * (payload limit + 8192) <= 64 MiB`
bytes, so in-flight deliveries plus prefetched batches stay a small multiple
of that bound. Startup requires the source stream's message limit to fit one
envelope. Network operations consume at most 5 seconds and no more than their
caller's remaining deadline. Operators own streams, retention, capacity,
replicas, discard policy, and broker deduplication windows; the broker enforces
subjects, stream binding, and message sizes on every publication.

## Configure, operate, and remove

Only `jobs-worker` connects to NATS; the API publishes through the
PostgreSQL outbox and never opens a broker connection. Consumer mode needs
complete source, consumer, and DLQ configuration. Startup requires NATS
JetStream >= 2.12.3, validates the named streams and the source message limit,
and fails with sanitized configuration, authentication, connection, topology,
bounds, or timeout reasons. A refused stream or durable request also logs
`messaging_admission_failed` with the request, that reason, and the broker's
numeric JetStream error code; the broker's description can quote configuration
and is not logged. The connection carries the worker's identity as its NATS
client name. Readiness uses the existing refresher and reads
only local connection state; a lost connection fails its next evaluation. On shutdown, readiness drains, pulls
stop, admitted handlers settle under the existing shared deadline, application
tasks join, and dependency close waits for the NATS closed event. A forced drain
is degraded, never a clean completion.

Production topology requires R3 replicas across independent failure zones and
`sync_interval: always`. R1 is only for local development and tests. Choosing a
different sync interval requires a named operator's explicit acceptance of the risk
that acknowledged data can be lost; startup cannot certify those deployment
properties. Adapter telemetry has only closed publication, handler, DLQ, and
connection result vocabularies, plus counters for pull-stream errors and
failed settlements. It never labels metrics or logs with payloads,
credentials, arbitrary errors, or event IDs.

Publication runs in a `messaging_publish` producer span and writes that span's
W3C `traceparent` and `tracestate` into the message headers, as the Go
template does; no other propagation field is written. An admitted delivery runs
its handler and settlement in a `messaging_process` consumer span whose parent
is the publisher's span, so one trace covers the outbox job, the publication,
and the handler. A delivery without a valid trace context starts its own
trace. Both spans carry the subject and the closed outcome. A failed
publication logs `messaging_publish_failed`, and a handler result other than
success logs `messaging_delivery_failed` with the subject, the delivery
attempt, and the outcome. `HandlerError` carries no cause; a handler logs its
own cause inside the delivery span, where the record shares the trace.

To remove the profile, initialize or migrate a service with `MESSAGING=none` so
the initializer removes its code, configuration, tests, images, CI, and this
guide together. Do not remove a profile from a live service by deleting only a
binary or configuration section.

## Allocation and CPU measurements

The adapter avoids temporary header strings and a cloned dispatch key.
Schema and subject validation retain their admission
rules. Deterministic transfer IDs use the same SHA-256 input bytes and lowercase
hex output.

### Measurement scope

Measurements ran on 2026-09-28 against baseline
`9631b0020e9efbf5df5026e005d898d083ce0db5`, on one DigitalOcean c-4 in London:
four vCPU, 8 GiB RAM, Intel Xeon Platinum 8168, Ubuntu 24.04, Rust 1.98.1 and
the locked dependencies. A normal downstream binary imported `infra_messaging`;
baseline and candidates used identical release features and were saved before
test builds. Timings used three warmups and 21 alternating pairs, pinned to one
CPU. Payloads contained an ASCII data string and a number; sizes below describe
the data string, not the complete JSON envelope.

| Public API operation | Baseline median | Optimized median |
| --- | ---: | ---: |
| Prepare, 64-byte data string | 1.94 µs | 1.41 µs |
| Prepare, 4,096-byte data string | 5.78 µs | 4.10 µs |
| Prepare, 48,000-byte data string | 47.46 µs | 32.84 µs |
| Encode normal headers | 1.60 µs | 1.13 µs |
| Decode normal envelope | 1.27 µs | 0.69 µs |
| Decode with extra headers | 1.96 µs | 0.64 µs |
| Deterministic DLQ ID | 1.51 µs | 0.85 µs |

These are per-operation averages within each process, then medians across runs.
The prepare result is sensitive to compiled context; it is not a claim that
subject scanning alone accounts for the large-payload difference. Performance
on other compilers, architectures and payload shapes needs fresh measurement.

Separate calibrated allocation runs measured normal decode at 19→6 allocations
and 311→95 requested bytes per operation. Moving the dispatch
key and streaming the DLQ hash each removed one allocation in component probes;
their isolated CPU gains were not established reliably.

Requested allocation bytes include reallocation requests and describe allocation
traffic, not retained heap or RSS. Process maximum RSS remained approximately
5 MiB for the component workloads; no RSS reduction was established.

### Real broker boundary and correctness

The broker comparison used the pinned NATS 2.15.0 image, R1 file storage,
`sync_interval: always`, loopback plaintext, two Tokio workers, 16 in-flight
publications and consumer concurrency 16. The handler performed no business
work and no metrics recorder was installed. Each of 11 paired samples per size
waited for all 1,000 handlers and the durable ACK floor.

There was no demonstrated total-delivery improvement at 64 bytes or 4 KiB.
For 48,000-byte data strings, paired total time decreased about 3.2%, with a
paired-bootstrap 95% time-ratio interval of 0.931–0.985. This synthetic R1 result
does not establish production R3 capacity, TLS cost, database-outbox performance,
or a general broker-throughput improvement.

The measured full-template candidate passed its crate unit suite, twelve real JetStream tests
and explicit compatibility checks against the pinned actual Go package in both
directions. Schema admission, representative header/subject bounds and transfer
identities also matched differential probes. Delivery, retry, acknowledgment,
cancellation and resource limits retain the messaging contract above.

The research retained an intermediate source-included prepare regression and an
invalid confirmation series whose binary had been overwritten by a test-feature
build. Neither supplies the figures above. The public-library confirmation used
uniform build features, and an independent review reproduced its 31 timing
cells, 66 broker samples, allocation summaries and RSS ranges from raw receipts.
The original experimental patch SHA-256 was
`c76e3b3318b94ad8bae054edfedc14c7caa7c38c255414ed8109a9465ecd511f`;
delivery preserves these operations while keeping extension-only hashing with
its optional module. The temporary host was deleted after evidence download.

### Publication path

Preparing an event checks the header rules without building the header map
that publication builds again. NATS header names are constants, so inserts and
lookups no longer validate and copy each name. Route keys borrow the payload
type's static event type, and the prepared event borrows it too. Publication
and handler outcome metrics are registered once at connect, so the metrics
recorder must be installed before `Messaging::connect`; the bootstraps do so.

Measured on 2026-09-28 on a DigitalOcean c-4 in London (Intel Xeon Platinum
8280, Ubuntu 24.04, Rust 1.98.1, locked dependencies) against the adapter
above. Each figure is the median of 15 interleaved rounds pinned to one CPU.
The event is an order with one line item (203-byte JSON) or 40 items (2.8 KB).

| Operation | Before | After |
| --- | ---: | ---: |
| Prepare, one item | 1.55 µs, 24 allocations | 0.52 µs, 7 allocations |
| Prepare and encode headers, one item | 2.62 µs | 1.50 µs |
| Prepare and encode headers, 40 items | 5.29 µs | 4.14 µs |
| Decode normal envelope | 0.46 µs, 6 allocations | 0.35 µs, 2 allocations |
| Outcome counter and histogram, Prometheus recorder | 353 ns, 6 allocations | 77 ns, 0 allocations |

Retired instructions agree: prepare and encode fell from 27,859 to 16,174 per
event. Against a local NATS 2.15.0 JetStream (memory storage, two client Tokio
workers, Prometheus recorder installed, 64 publications in flight, 10 paired
rounds), one-item publication throughput rose 12% and client CPU per event fell
15%. With one publication in flight, client CPU fell 7.5% and latency changed
by 2–5%. These results do not establish production R3 or TLS capacity.

Rejected alternatives: a 1 KiB initial serialization buffer (slower for large
payloads, more memory for small ones), an ASCII fast path for header text
validation (no measurable change), a non-generic `prepare` core (about 550
bytes less code per payload type but no faster), sharing route subject bytes
through `async_nats::Subject` (about 1% fewer instructions, public signature
change), and one route-and-handler map (about 40 ns per delivery).

### Identity checks and lookups

The payload type's event type is checked at compile time, so `prepare` checks
only the event ID, once, and the occurrence time. Header text is checked byte
by byte: a control character is a byte below 0x20, 0x7F, or 0xC2 followed by
0x80–0x9F, which are exactly the characters `char::is_control` rejects. Decoding
reads the five identity headers and the encoded size in one pass over the
header map; each `HeaderMap::get` would hash the name with SipHash. The
registry maps use foldhash: every key is a payload type's constant, so no
caller can choose colliding keys.

Measured on 2026-09-29 on a DigitalOcean c-4 in London (Intel Xeon Platinum
8280, Ubuntu 24.04, Rust 1.98.1, fat LTO, locked dependencies) with the
events above: medians of 15 interleaved rounds pinned to one CPU. Both sides
were built with 64-byte function alignment and
`-x86-branches-within-32B-boundaries`. Without it, a change that does not touch
`prepare` moved the 40-item and 450-item prepare by 11%, from code layout alone.

| Operation | Before | After | Instructions |
| --- | ---: | ---: | ---: |
| Prepare, one item | 521 ns | 428 ns | −15% |
| Prepare and encode headers, one item | 1.35 µs | 1.27 µs | −7% |
| Decode normal envelope | 316 ns | 169 ns | −39% |
| Decode and dispatch, one item | 1.03 µs | 0.85 µs | −21% |
| Decode and dispatch, 40 items | 7.64 µs | 7.47 µs | −2% |
| Route lookup | 30 ns | 10 ns | −66% |

Allocations did not change. Larger events spend their time in `serde_json`.

Rejected alternatives: sonic-rs for payload JSON (30% less consume CPU and 7%
less prepare CPU for 40 items, but it serializes a `serde_json::value::RawValue`
field as a private marker object, cannot deserialize one, reads `-0.0` as `0.0`
and rounds some decimals differently), building the `Event-Schema` value
without `format!` (no change), and mimalloc as the global allocator (15–20% less
CPU for one-item events, but it is a binary-wide choice with a larger resident
set, measured with the HTTP path).

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

### Outbox allocation measurements

The outbox metadata-only payload-limit calculation avoids base64 encoding and
copying the payload. In the measurement environment described above, the public
helper with a 48,000-byte data string took 31.17→0.59 µs and used
129,164→1,100 requested bytes per operation. The enqueue metadata-budget helper
separately removed one payload copy. These component results do not establish
database or complete-outbox throughput, or a reduction in process RSS.

The publisher checks that the stored payload is syntactically valid JSON with
`serde::de::IgnoredAny`, which builds no value tree. The bytes were written by
`serde_json` when the event was prepared, so this guards a row edited in
place; it is not a second validation of the event.
<!-- template:end outbox:docs-durable-messaging-outbox -->
