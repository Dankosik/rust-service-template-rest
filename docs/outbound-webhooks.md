# Outbound webhooks

<!-- template:begin webhooks:docs-outbound-webhooks-guide -->
The `WEBHOOKS=durable` profile is an inert, reusable static-endpoint capability.
It requires `DATABASE=postgres`, `JOBS=postgres`, and `OUTBOUND_HTTP=bounded`
at initialization. Selection starts no worker, creates no endpoint, and makes no
request until an adopter prepares and enqueues a delivery in its business flow.

## Static endpoints and key custody

Endpoint metadata and signing keys are a process-start snapshot. The URL is
non-secret; each endpoint's required current key and optional predecessor key
are environment-only and redacted.

```toml
[webhooks.endpoints.partner]
url = "https://hooks.example.test/events?tenant=blue"
# secret and previous_secret may appear here only as empty placeholders.
```

```sh
APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET=whsec_<base64-key>
APP__WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_SECRET=whsec_<base64-key>
```

Endpoint IDs are non-secret, nonempty, NUL-free values. The required secret and
an explicitly supplied predecessor cannot be blank. Provider construction
admits the URL and a nonempty decoded Standard Webhooks base64 key, with
optional `whsec_` prefix. The
recommended 24--64 random-byte range is provisioning guidance, not a second
trusted-input rule; a 32-byte random key is an appropriate example.

No secret belongs in TOML, a job payload, metrics, logs, URL parameters, or an
application endpoint manifest. There is no management API, remote secret
provider, global repeated-secret registry, or endpoint lookup from webhook data.
Rotation takes effect on restart: set the new `secret`, retain the former value
as `previous_secret` while receivers accept both, then remove it and restart.

## Transactional acceptance and delivery

An adopter resolves one endpoint and supplies final body bytes and a content
type before its business transaction. The default content type is
`application/json`. The caller enqueues through its current
`&mut infra_postgres::Tx` and propagates an enqueue failure, so its business
effect and job insert commit or roll back together. An acknowledged commit is
the acceptance boundary. Unknown commit acknowledgement is uncertainty for the
business owner, never permission to replay a transaction or claim no delivery.
There is no webhook acceptance ledger, fan-out replay API, or business
idempotency owner.

Construct `Outbound` once from the non-secret configured endpoint map. A
producer prepares before entering its business transaction, then stages only
the already-prepared delivery on the supplied transaction:

```rust,ignore
let outbound = Outbound::new(endpoints)?;
let delivery = outbound.prepare(endpoint_id, body, content_type)?;
let job_id = delivery.enqueue(tx).await?;
```

Here `endpoints` is `BTreeMap<String, Endpoint>` containing admitted destination
metadata only. The returned `JobId` is the stable Standard Webhooks message ID.
This is provider wiring, not a template business event or consumer.

The worker decodes each endpoint's current and optional predecessor keys before
claims, builds one dispatcher from that current snapshot, and consumes it into
the existing kind registry:

```rust,ignore
let dispatcher = outbound.dispatcher(signing_keys)?;
dispatcher.register(kinds);
```

`Dispatcher::register` installs `webhooks.deliver` with `DELIVERY_POLICY` (20
attempts and 30 seconds). Producers never resolve signing secrets. New jobs use
the version-2 common payload of endpoint ID, content type, and body only. The
reader accepts version 1 but ignores its obsolete destination and key-reference
fields; they cannot route or sign a delivery. An endpoint missing from the
current worker snapshot is retryable and spends an attempt.

The payload stores endpoint ID, final bytes, and content type. Bodies are capped
at 128 KiB; the existing JSONB-size check is final. Base64 preserves arbitrary
bytes including NUL and non-UTF-8. The durable job ID is the stable `webhook-id`;
each retry regenerates timestamp/signature but reuses that ID and body. Current
URL and keys apply to all attempts after restart.

## Standard Webhooks and transport

Attempts are HTTPS `POST` with `webhook-id`, `webhook-timestamp`, and
`webhook-signature`. v1 HMAC-SHA256 signs exact bytes:

```text
message-id + "." + canonical-decimal-timestamp + "." + raw-body
```

The active key and optional predecessor emit one or two space-separated v1
signatures. Do not emit key, signature, payload, URL, or endpoint ID as a
diagnostic value or metric label.

The shared protocol API is `SigningKey::from_encoded`, `KeyRing::new` (or
`KeyRing::from_encoded`), and `KeyRing::signatures(message_id, timestamp, body)`
for outbound headers. `SigningKey` only admits redacted base64 key material at
construction. `KeyRing::verify(&HeaderMap, body, SystemTime)` returns a
`VerifiedMessage` with original message ID and parsed timestamp for receivers.
`MAX_BODY_BYTES` is the fixed 128 KiB boundary.

The provider reuses [aws-lc-rs HMAC
1.18.1](https://docs.rs/aws-lc-rs/1.18.1/aws_lc_rs/hmac/index.html) and base64
rather than published [standardwebhooks
1.0.1](https://crates.io/crates/standardwebhooks/1.0.1): that release requires
UTF-8 input, controls its clock, uses overflow-sensitive subtraction, and has a
handwritten comparison. A wrapper cannot correct all four; a fork adds provenance
cost without closing every gap. RustCrypto remains viable but adds a second
primitive owner without current benefit. Reopen if a published library closes the
named gaps or the retained Cargo graphs expose a concrete aws-lc backend drawback.
The interoperable wire authority is the [Standard Webhooks
specification](https://github.com/standard-webhooks/standard-webhooks/blob/bece768d960f09e242f5cd5686d859e475d6b478/spec/standard-webhooks.md).

The outbound snapshot builds one fixed-authority client and one decoded key ring
for each configured endpoint before claims. The client owns hostname TLS
verification, pooling, no proxy/redirect, and response bounds. Its attempt
telemetry carries the method, receiver host and port, status, and a static
outcome, never the path, query, headers, or body. Endpoint URLs are trusted
operator configuration: HTTPS without credentials or fragments, including
private addresses the deployment trusts; existing paths, query strings, and
HTTPS ports remain supported. The client is not an SSRF boundary, so never
populate the endpoint map from tenant, request, or payload data. There is no
historical client cache or saved-routing revalidation.

The jobs deadline bounds signing, transport, and response reading to 30 seconds.
A complete bounded 2xx completes delivery; 410 is a permanent `endpoint_gone`
outcome with an operator warning. Every other HTTP status, plus network,
timeout, DNS, and response-read failures, retries with the stable ID. Valid
`Retry-After` delta-seconds or HTTP-date is a jobs delay floor capped at 24h;
malformed, elapsed, or conflicting advice uses ordinary backoff. Each endpoint
allows one active exchange. At capacity, it performs no HTTP I/O and returns the
one-second `AtCapacity` snooze, refunding the attempt and freeing the worker
slot. With at least two worker slots, one slow endpoint cannot occupy every slot;
one worker remains serial. This is neither a fairness nor a cross-process
capacity guarantee. Jobs alone owns jitter, leases, delay, exhaustion, and retry.

## Raw-byte interoperability vector

This non-secret vector supplements the upstream ordinary-JSON vector and is a
fixed interoperability input; it does not by itself prove a runtime path.

| Field | Value |
| --- | --- |
| Key bytes | consecutive `00` through `1f` |
| Encoded key | `AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=` |
| Message ID | `msg_test` |
| Timestamp | `1700000000` |
| Raw body hex | `007b2278223a317dff` |
| Signature | `v1,zS3Ns419EcpSLc66f4eI2flsBpaIFaRVByOzialbunY=` |
| Body SHA-256 | `377e14aa8ca8feef004afa0a23907d26df7c456a3e045e23cb2f1dbc8cc44102` |

## Rollout and evidence

Apply the migrations retained by the selected profile. Before any producer emits
version-2 jobs, stop and fence every old worker and old producer, then start a
new worker with the converted endpoint snapshot and only afterward enable v2
producers. An old worker must not resume after the v2 cutover. The receipt
migration belongs only to the inbound profile.

The v1 reader remains while any v1 job can be claimed, including retained or
replayable terminal jobs, and while any old producer can still write one. Remove
it only in a separately reviewed change after those conditions are demonstrably
gone; do not bulk-rewrite durable payloads. Rollback stops new producers and
drains relevant live jobs before removing capable workers; pending work or
unknown commit state requires rolling forward. Provider registration, rotation
execution, endpoint ownership, and egress certification remain operational work
outside this guide. Jobs owns attempt/queue telemetry; this profile adds no
delivery observer, health loop, automatic pause, deletion, notification channel,
or retention ledger.
<!-- template:end webhooks:docs-outbound-webhooks-guide -->
