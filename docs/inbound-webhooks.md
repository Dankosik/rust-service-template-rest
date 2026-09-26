# Inbound webhooks

<!-- template:begin inbound-webhooks:docs-inbound-webhooks-guide -->
The `INBOUND_WEBHOOKS=standard-webhooks` profile exposes durable receipt and
processing for authenticated Standard Webhooks v1 notifications. It requires
`DATABASE=postgres` and `JOBS=postgres`; every profile defaults to `none`. An
empty endpoint map is inert. An active endpoint needs a pool and an explicit
consumer binding in the composition root, so partial wiring fails startup rather
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
is nonempty and cannot contain the signing dot. Conflicting identity/timestamp
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
| First verified endpoint/message ID/body | one receipt and one processing job commit atomically, then `204` |
| Same endpoint/message ID/exact body | `204`; no extra job, even after processing |
| Same endpoint/message ID/different body | `409 webhook_conflict`; original receipt/job unchanged |
| Database or commit acknowledgement unavailable/unknown | `503`; no false `204`, sender retries same identity/body |

A duplicate still passes current signature/timestamp verification; deduplication
is not authentication bypass. Identity is raw bytes, so JSON whitespace differs;
content type alone does not. Existing overload, timeout, header-limit, panic,
method, and server outcomes remain effective contract behavior.

## Receipt and consumer processing

The provider API is `infra_webhooks::inbound::{Receiver, ReceiptOutcome,
ReceiveError, Incoming, Consumer, Consumers, Processor}`. Construction uses
`Receiver::new(PgPool, endpoint_key_rings)`; admission is
`receive(endpoint_id, &HeaderMap, body, SystemTime)` and returns Accepted,
Duplicate, or Conflict, or closed UnknownEndpoint, Rejected, or Unavailable
errors. `Incoming` exposes original endpoint, message-ID, body, and optional
byte-safe content type. A registered `Consumer` receives `(&mut Tx, &Incoming)`
and returns a boxed Send future of `Result<(), JobError>`; `Processor` owns the
binding lookup, transaction, and fenced completion.

A derived service builds one explicit registry constructor with its
`Arc<dyn Consumer>` adapters and uses that constructor in both roots: the service
checks the configured endpoint binding before listener admission, and the worker
moves its result into `Processor::new` before registering `webhooks.process`.

```rust,ignore
let mut consumers = Consumers::new();
consumers.insert(endpoint_id, Arc::clone(&consumer));
// service: verify consumers.contains(endpoint_id) before admitting ingress
// worker: Processor::new(consumers) registers the processing kind
```

The template deliberately supplies an empty registry because it has no business
consumer. It is not a successful default: active ingress without the derived
service's binding fails startup, and a rolling worker configuration gap snoozes
the durable job without consuming an attempt.

PostgreSQL arbitrates concurrent deliveries. In one transaction it inserts a
receipt and enqueues `webhooks.process`. Receipt identity hashes a domain tag and
length-prefixed endpoint/message identifiers, while stored full identity remains
the comparison authority. A conflict compares that identity and body SHA-256:
equal body is duplicate, different body is conflict, and an identity-hash
collision with a distinct full identity is a sanitized `503` integrity outcome.
The receipt keeps only endpoint, message identity, and body fingerprint; the job
holds raw body.

The worker uses existing jobs policy (25 attempts, 60 seconds), resolves a
consumer before opening a transaction, and snoozes a missing binding for 60
seconds without spending an attempt. Database effects and
`complete_in_tx(&mut Tx)` share a transaction, so a stale claim or consumer
error rolls back business effects. An unknown commit becomes ordinary retryable
failure without replaying that transaction closure; a fenced later outcome
cannot undo an already committed completion. An external consumer must supply
recipient idempotency from endpoint/message identity because PostgreSQL cannot
roll back its effect.

Receipts have no expiry here: distinct identities grow receipt metadata and
deleting it permits reacceptance. This profile does not certify payload erasure,
legal retention, provider registration, capacity, TLS ingress, or production
operation. Terminal job retention remains jobs-owned and cannot delete live work.

## Rollout and observation

Apply the additive migration, deploy compatible workers with bindings, then
enable ingress. Rollback stops ingress/producers and drains affected live work
before removing capable workers; pending work or unknown commit state means roll
forward. PostgreSQL stays the readiness/shutdown dependency; no sender network
probe gates startup.

Incoming telemetry has bounded outcomes: accepted, duplicate, rejected,
conflict, unavailable, and unknown endpoint. Logs have closed missing-binding,
missing-secret, and delivery-classification reasons. Never use endpoint IDs,
webhook IDs, URLs, payloads, signatures, secrets, or arbitrary errors as labels
or diagnostic values. Jobs owns queue/attempt telemetry; no second webhook
worker, lifecycle, or delivery observer exists.
<!-- template:end inbound-webhooks:docs-inbound-webhooks-guide -->
