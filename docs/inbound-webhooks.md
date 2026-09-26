# Inbound webhooks

<!-- template:begin inbound-webhooks:docs-inbound-webhooks-guide -->
The `INBOUND_WEBHOOKS=standard-webhooks` profile exposes durable receipt and
processing for authenticated Standard Webhooks v1 notifications. It requires
`DATABASE=postgres` and `JOBS=postgres`; every profile defaults to `none`. An
empty endpoint map is inert. An active endpoint needs a pool and an explicit
consumer binding in the shared adopter registry, so partial wiring fails startup rather
than accepting work into a successful no-op.

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

Keys are Standard Webhooks base64, optionally `whsec_` prefixed, and decode to
nonempty bytes. The 24--64-byte recommendation is provisioning guidance; a
32-byte random key is an example, not an extra startup rule. Endpoint/key scope
prevents verification with another endpoint's key. The processing worker needs
the non-secret binding only, not the verification keys.

Endpoint IDs are nonempty and NUL-free; no slash, query, fragment, or slug
restriction is imposed on trusted configuration. The router percent-encodes an
ID as one path segment (`partner/a?#` becomes `partner%2Fa%3F%23`) and resolves
the decoded identity for binding and verification.

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
is 1–255 bytes for new receiver admission and cannot contain the signing dot.
The 255-byte bound is an application storage constraint, not a protocol limit.
Historical jobs with longer IDs remain processable. Conflicting identity/timestamp
headers reject; identical repeats are allowed. Candidates are space-delimited:
unknown versions and mismatches do not prevent another valid v1 candidate with a
current or predecessor key.

The common protocol surface constructs a `SigningKey` with
`SigningKey::from_encoded`, then `KeyRing::new` or `KeyRing::from_encoded`.
`KeyRing::verify(&HeaderMap, body, SystemTime)` returns a `VerifiedMessage` whose
message ID is original bytes and timestamp is the parsed integer. `MAX_BODY_BYTES`
is the fixed 128 KiB boundary. Key material is redacted after construction.

| Request | Response and durable effect |
| --- | --- |
| Unknown endpoint | `404` problem; no receipt/job |
| Missing, malformed, stale/future, or invalid evidence | `400 webhook_rejected`; no durable effect |
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

The provider API is `infra_webhooks::inbound::{Receiver, ReceiptOutcome,
ReceiveError, Incoming, Consumer, Consumers, Processor}`. Construction uses
`Receiver::new(PgPool, endpoint_key_rings)`; admission is
`receive(endpoint_id, &HeaderMap, body, SystemTime)` and returns Accepted,
or Duplicate, or closed UnknownEndpoint, Rejected, or Unavailable
errors. `Incoming` exposes original endpoint, message-ID, body, and optional
byte-safe content type. A registered `Consumer` receives `(&mut Tx, &Incoming)`
and returns a boxed Send future of `Result<(), JobError>`; `Processor` owns the
binding lookup, transaction, and fenced completion.

A derived service edits `consumers()` in `crates/webhook-consumers/src/lib.rs`
to register its `Arc<dyn Consumer>` adapters. Both roots call that one constructor
and check every configured endpoint before serving or claiming. The worker
moves that same registry into `Processor::new` when registering
`webhooks.process`; it does not construct a second registry.

```rust,ignore
pub fn consumers() -> Consumers {
    let mut consumers = Consumers::new();
    consumers.insert(endpoint_id, Arc::clone(&consumer));
    consumers
}
```

The template deliberately supplies an empty registry because it has no business
consumer. It is not a successful default: active ingress without the derived
service's binding fails startup in both processes. A historical queued job whose
binding is no longer configured retries and spends its normal attempt budget.

PostgreSQL arbitrates concurrent deliveries. In one explicit READ COMMITTED
transaction it inserts a receipt and enqueues `webhooks.process`. The composite
primary key uses C-collated endpoint text and binary message IDs. Only the first
insert enqueues a job; an authenticated duplicate leaves the original job body
and content type unchanged. A new receipt's database-default `received_at`
records its first admission and never refreshes on replay. Its time index
supports future maintenance, without introducing a TTL or cleanup task.

The worker uses existing jobs policy (25 attempts, 60 seconds), resolves a
consumer before opening a transaction, and retries a missing binding until
the normal attempt budget is exhausted. Database effects and
`complete_in_tx(&mut Tx)` share a transaction, so a stale claim or consumer
error rolls back business effects. Unknown commit uses the existing
transaction-unknown outcome without a competing supervisor transition. An
external consumer must supply recipient idempotency from endpoint/message
identity because PostgreSQL cannot roll back its effect.

Receipts have no expiry here: distinct identities grow receipt metadata and
deleting it permits reacceptance. This profile does not certify payload erasure,
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

Incoming telemetry has bounded outcomes: accepted, duplicate, rejected,
unavailable, and unknown endpoint. Logs have closed missing-binding,
missing-secret, and delivery-classification reasons. Never use endpoint IDs,
webhook IDs, URLs, payloads, signatures, secrets, or arbitrary errors as labels
or diagnostic values. Jobs owns queue/attempt telemetry; no second webhook
worker, lifecycle, or delivery observer exists.
<!-- template:end inbound-webhooks:docs-inbound-webhooks-guide -->
