# Inbound webhooks

<!-- template:begin inbound-webhooks:docs-inbound-webhooks-guide -->
The `INBOUND_WEBHOOKS=standard-webhooks` profile exposes durable receipt and
processing for authenticated Standard Webhooks v1 notifications. It requires
`DATABASE=postgres` and `JOBS=postgres`; every profile defaults to `none`. An
empty endpoint map is inert. An active endpoint needs a pool in the service and
an explicit consumer binding in the worker, so partial wiring fails startup
rather than accepting work into a successful no-op.

Verification covers the symmetric `v1` scheme (HMAC-SHA256) only. A sender
with its own signature scheme, such as Stripe or GitHub, cannot use this
route, and neither can the asymmetric `v1a` (Ed25519) scheme. Either is a
separate verification capability, not a consumer binding.

## Static receiving bindings

Receiving endpoints form a restart-applied snapshot. Endpoint metadata binds a
stable ID to active and optional predecessor secret references; key bytes remain
environment-only:

```toml
[inbound_webhooks.endpoints.partner]
active_key = "partner_v2"
previous_key = "partner_v1"
```

```sh
APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V2=whsec_<base64-key>
APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V1=whsec_<base64-key>
```

Keys are Standard Webhooks base64, optionally `whsec_` prefixed, and must decode
to 24--64 bytes. A 32-byte random key is an example. Endpoint/key scope
prevents verification with another endpoint's key. The processing worker needs
the non-secret binding only, not the verification keys.

Endpoint IDs in a file and key references follow the
[file-key rule](configuration-source-policy.md#source-of-truth): lowercase
letters, digits, `_`, and `-`, without `__` or a trailing `_`, so an `APP__`
variable addresses the same entry. The router percent-encodes an ID as one
path segment and resolves the decoded identity for binding and verification.

Rotate by adding a new immutable reference, switching active/predecessor
references, retaining old values while old work needs them, then restarting.
There is no remote secret provider, request-supplied endpoint or key lookup,
global secret registry, default consumer, or provider-owned payload schema.

## Exact-byte admission

The retained profile adds `POST /webhooks/{endpoint_id}`. It supplies OpenAPI
`security: []` to override root bearer authentication while its required
Standard Webhooks headers describe a mandatory signature check. The route does
not ask providers for an access token and disappears when the profile is absent.

The OpenAPI request body declares raw `*/*` content without a JSON schema;
clients must send the original bytes, not a JSON array of byte values.

The receiver limits the original body to 128 KiB before verification or writes.
It verifies raw bytes rather than application JSON. v1 HMAC-SHA256 signs the
original message-ID bytes, ASCII dot, parsed timestamp rendered as canonical i64
decimal, ASCII dot, and original body bytes. The parser accepts optional sign
and leading zeroes, rejects whitespace/out-of-range values, compares in a wider
integer domain, and accepts the inclusive 300-second clock window. A message ID
is 1–255 bytes and cannot contain the signing dot. The protocol rejects a longer
ID. Historical jobs already queued with longer IDs remain processable. Conflicting identity/timestamp
headers reject; identical repeats are allowed. Candidates are space-delimited:
unknown versions and mismatches do not prevent another valid v1 candidate with a
current or predecessor key. Verification computes the expected tag once per key
and compares every candidate against it in constant time, so one request costs
at most two HMACs over the body however many candidates its headers carry.

The common protocol surface constructs a `SigningKey` with
`SigningKey::from_encoded`, then `KeyRing::new` or `KeyRing::from_encoded`.
`KeyRing::verify(&HeaderMap, body, SystemTime)` returns a `VerifiedMessage` whose
message ID is original bytes and timestamp is the parsed integer. `MAX_BODY_BYTES`
is the fixed 128 KiB boundary. Key material is redacted after construction.

| Request | Response and durable effect |
| --- | --- |
| Unknown endpoint | `404` problem; no receipt/job |
| Missing, malformed, stale/future, or invalid evidence | `400 webhook_rejected`; no durable effect; reason logged and counted |
| Body above 128 KiB | `413`; no durable effect |
| Other body-read failure or message ID over 255 bytes | `400 webhook_rejected`; no durable effect |
| First verified endpoint/message ID | one receipt and one processing job commit atomically, then `204` |
| Same endpoint/message ID, including changed body or content type | `204`; no extra job, original payload stays authoritative, even after processing |
| Database or commit acknowledgement unavailable/unknown | `503`; no false `204`, sender retries same identity/body |

A duplicate still passes current signature/timestamp verification; deduplication
is not authentication bypass. Identity is the exact endpoint and raw message-ID
bytes; replay never replaces the first accepted body or content type. Existing overload, timeout, header-limit, panic,
method, and server outcomes remain effective contract behavior.

## Receipt and consumer processing

The provider API is `infra_webhooks::inbound::{Receiver, Verifier, Rejection,
ReceiptOutcome, ReceiveError, Incoming, Consumer, Consumers, Processor}`.
Construction uses `Receiver::new(PgPool, endpoint_verifiers)`; admission is
`receive(endpoint_id, &HeaderMap, body, SystemTime)` and returns Accepted
or Duplicate, or closed UnknownEndpoint, Rejected, or Unavailable
errors. `Rejected` carries the verifier's `Rejection`, a static reason label.
`Incoming` exposes original endpoint, message-ID, body, and optional
byte-safe content type. A registered `Consumer` implements
`async fn process(&self, &mut Tx, &Incoming) -> Result<(), JobError>` under the
re-exported `#[infra_webhooks::inbound::async_trait]`; `Processor` owns the
binding lookup, transaction, and fenced completion. `Consumers::insert` refuses
a second binding for one endpoint, and `Consumers::require` fails startup when
a configured endpoint has no binding. `Processor::register` installs
`webhooks.process` with the default jobs policy.

A derived service binds its `Arc<dyn Consumer>` adapters in `register` in
`crates/jobs-worker/src/main.rs`, beside its job kinds and message handlers.
The worker checks every configured endpoint before claiming and moves the
registry into `Processor::new(...).register(kinds)`. The service process holds
no consumer: it admits a verified delivery durably, and the worker that
refuses to start without the binding is the signal that nothing processes it.

```rust,ignore
use std::sync::Arc;

use infra_jobs::JobError;
use infra_postgres::Tx;
use infra_webhooks::inbound::{Consumer, Consumers, Incoming, async_trait};

struct Partner;

#[async_trait]
impl Consumer for Partner {
    async fn process(&self, tx: &mut Tx<'_>, incoming: &Incoming) -> Result<(), JobError> {
        // Parse incoming.body() and apply the business effect on tx.
        Ok(())
    }
}

// In `register`:
let mut consumers = Consumers::new();
consumers.insert("partner", Arc::new(Partner))?;
```

The transaction, and its pooled connection, stay open until `process` returns.
Keep `process` to database effects; for an effect outside PostgreSQL, enqueue a
job on `tx` and let that job make the call.


### Providers that sign another way

Each endpoint authenticates its sender through a `Verifier`:
`verify(&HeaderMap, body, SystemTime) -> Result<Bytes, Rejection>` returns the
provider's stable message identity or a static rejection reason. `KeyRing` is
the Standard Webhooks verifier and the only one the template wires. A provider
with its own signature scheme (a different header, digest, or an identity
carried in the body) implements `Verifier` in the derived service and takes
that endpoint's place in `prepare` in
`crates/service/src/bootstrap/webhooks.rs`; a receiver that mixes
schemes takes each endpoint as `Arc<dyn Verifier>`. The receipt, duplicate
arbitration, job, and consumer path are unchanged. The receiver refuses an
identity outside 1--255 bytes because the receipt key is indexed, and the
128 KiB body bound still applies. A verifier never puts request, signature, or
key bytes in its reason, and it bounds its own work per request.

The template deliberately supplies an empty registry because it has no business
consumer. It is not a successful default: a worker refuses to start while a
configured endpoint has no binding. The service checks no binding and keeps
admitting; its receipts wait for a worker that has one. A historical queued job
whose binding is no longer configured retries and spends its normal attempt
budget.

Every endpoint shares the one `webhooks.process` kind and the worker's job
slots, so one endpoint's slow consumer delays the others. Deliveries of one
endpoint run concurrently and in no guaranteed order.

PostgreSQL arbitrates concurrent deliveries. In one explicit READ COMMITTED
transaction it inserts a receipt and enqueues `webhooks.process`. The composite
primary key uses C-collated endpoint text and binary message IDs. Only the first
insert enqueues a job; an authenticated duplicate leaves the original job body
and content type unchanged. A new receipt's database-default `received_at`
records its first admission and never refreshes on replay. The service process
deletes receipts older than 7 days, in batches, every 60 seconds, starting at
boot. A sender retries one message ID with fresh timestamps for its whole retry
horizon (Standard Webhooks senders retry for more than a day; this template's
outbound schedule runs about six days), so receipts must outlive that horizon.
The specification's 5-minute example only covers replay of one signed request.

The worker uses existing jobs policy (25 attempts, 60 seconds), resolves a
consumer before opening a transaction, and retries a missing binding until
the normal attempt budget is exhausted. Database effects and
`complete_in_tx(&mut Tx)` share a transaction, so a stale claim or consumer
error rolls back business effects. An unknown commit becomes ordinary retryable
failure without replaying that transaction closure; a fenced later outcome
cannot undo an already committed completion. An external consumer must supply
recipient idempotency from endpoint/message identity because PostgreSQL cannot
roll back its effect.

Receipts older than 7 days are deleted by that service-process cleanup; deleting
one permits reacceptance of that identity. This profile does not certify payload erasure,
legal retention, provider registration, capacity, TLS ingress, or production
operation. Terminal job retention remains jobs-owned and cannot delete live work.

## Rollout and observation

This forward migration requires a coordinated cutover: stop inbound admission,
all producers, and old workers; drain them and disable old restart controllers
before applying it. Keep the existing migration bytes unchanged. The migration
locks the receipt table, refuses duplicate exact pairs with a static diagnostic,
and constructs the actual composite index before dropping hash columns. An
unindexable historical pair aborts the complete migration without deleting or
truncating history; stop the rollout and revisit storage design. Historical
`received_at` values approximate migration time, not original arrival time.

Start only new workers with configured bindings, then new service/producers,
and reopen ingress. After migration success, old binaries cannot resume: repair
forward. An uncommitted failed migration leaves the old schema available for
restoring the previous binaries/configuration. PostgreSQL remains the readiness
and shutdown dependency; no sender network probe gates startup.

`webhook_ingress_outcomes_total` counts admissions with the bounded `outcome`
label: accepted, duplicate, rejected, unavailable, and unknown_endpoint. A
configured endpoint adds its ID as `endpoint`, and a rejection adds `reason`:
the verifier's label (`missing_header`, `conflicting_header`,
`invalid_message_id`, `invalid_timestamp`, `timestamp_out_of_window`,
`invalid_signature`) or the route's `body_too_large` and `body_unreadable`. A
steady `timestamp_out_of_window` points at clock skew, `invalid_signature` at a
wrong or rotated key. The `webhook_delivery_rejected`,
`webhook_receipt_unavailable`, and `webhook_processor_missing_binding` events
carry the same endpoint and reason as fields. `webhook_receipt_unavailable`
adds the driver's `sqlstate` and bounded `cause` when one failed.

Every endpoint shares the `webhooks.process` kind, so the jobs events and
metrics cannot say whose consumer failed. `webhook_consumer_incomplete` names
the endpoint when its consumer returns a failure or a snooze, with `permanent`
set for a permanent failure; the jobs event that follows in the same attempt
span carries the outcome and summary.

`webhook_receipt_cleanup_runs_total` counts cleanup runs by `outcome`
(`completed`, `failed`), and `webhook_receipt_cleanup_removed_receipts_total`
counts the receipts each committed batch deleted. Each service replica runs
the cleanup, so both add up across replicas. A failed batch logs
`webhook_receipt_cleanup_failed` with its `failure` class (`acquire`, `begin`,
`statement`, `commit`), `sqlstate`, and `cause`. A run that keeps failing lets
the receipt table grow; it changes neither readiness nor admission.

Endpoint IDs are operator configuration, so the configured set bounds the
label. A requested ID that matches no configured endpoint is caller-controlled:
it is counted as unknown_endpoint without an `endpoint` label and never logged.
Never use webhook IDs, URLs, payloads, signatures, secrets, or arbitrary errors
as labels or diagnostic values. Jobs owns queue/attempt telemetry; no second
webhook worker, lifecycle, or delivery observer exists.
<!-- template:end inbound-webhooks:docs-inbound-webhooks-guide -->
