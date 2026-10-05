# Technical design: move the admitted webhook body

Status: ready. Independent [Technical Design review](technical-design-review.md): PASS.
Authority: [ready Definition](definition-transition.md), [Intent](intent.md),
[Specification](spec.md), and the [accepted profiling report](../hotpath-profiling/report.md).
This document selects a mechanism; it makes no measured improvement claim.

## Decision and evidence

Move the HTTP adapter's already-collected `bytes::Bytes` into the inbound
payload after receipt arbitration. Change only `Incoming`'s private body storage
from `Vec<u8>` to `Bytes`. Preserve `Receiver::receive(..., body: &[u8], ...)`
and add `Receiver::receive_bytes(..., body: Bytes, ...)` for owners able to
transfer their existing buffer. Both delegate to one private admission method.
`infra-http::webhooks::receive` uses the owned entry point and moves its buffer.

The shared admission method carries a private borrowed-or-owned body value in
`inbound.rs`. Verification borrows its exact slice. Only the winning receipt
branch consumes it into `Incoming`: move an owned `Bytes`; copy a borrowed
slice with `Bytes::copy_from_slice`. Keep this conversion inside the existing
`Incoming::new` construction/profiling boundary. There is no eager body copy,
`Bytes::clone`, new allocation, preparation, or serializer call on the owned
path before arbitration. The carrier is a local representation of ownership,
not a second admission implementation or an exported abstraction.

All other `Incoming` fields, field order, Serde attributes, accessors, redacted
Debug, job kind, and version behavior remain unchanged. In particular retain
`#[serde_as(as = "Base64")]` for `body`; do not invoke `Bytes`' native Serde
representation. Keep `Consumer` and `Processor` signatures unchanged.
`infra-jobs::enqueue`, `prepare`, SQL, and serialized validation stay untouched.

Current primary source identities (Git blob hashes) inspected for this design:

| Source | Blob |
| --- | --- |
| `crates/infra-webhooks/src/inbound.rs` | `d32c1e5865b8a4ba83a45ab31aa7501071819eb9` |
| `crates/infra-http/src/webhooks.rs` | `c4b581f1a378a0e6b464fb7503375dce3b17c638` |
| `crates/infra-jobs/src/enqueue.rs` | `a1da3f1b40051dfc4c3c0347d9622a69905467ef` |
| `Cargo.lock` | `981eb9ec797d1d23d6d6effabc21d5a100ede398` |

CodeGraph and the current source show the HTTP adapter already collects into
`Bytes`, then lends a slice to the receiver. `Incoming::new` performs
`body.to_vec()` only after the receipt insert wins. This is the one
body-length allocation selected for removal. Moving the existing handle does
not allocate a replacement body or refcount block. The retained 64 KiB fixture
has a body slightly larger than 65,536 bytes; comparison uses its actual full
body length. Metadata copies remain as in the control.

The resolved `bytes` 1.12.1 source documents `Bytes` ownership and implements
`AsRef<[u8]>`, `From<Vec<u8>>`, and `copy_from_slice`. The resolved
`serde_with` 3.23.0 Base64 adapter serializes any `AsRef<[u8]>` and deserializes
through `TryFrom<Vec<u8>>`; the standard blanket conversion applies to `Bytes`.
It retains standard padded encoding and its existing padded/unpadded decoding
policy. Thus the same adapter continues to own the wire representation and
decoder acceptance; no crate, dependency feature, or lockfile change is needed.
These version-specific library sources were read from the local registry;
official references are [Bytes](https://docs.rs/bytes/1.12.1/bytes/struct.Bytes.html)
and [Base64](https://docs.rs/serde_with/3.23.0/serde_with/base64/struct.Base64.html).

## Alternatives and costs

| Candidate | Disposition and decisive constraint | Reopen evidence |
| --- | --- | --- |
| Move existing `Bytes`, retain a borrowed public entry point over one core | Selected: removes the measured body copy without touching shared job serialization or public caller compatibility. Cost: one additive entry point and one private ownership carrier. | Remote proof misses the allocation target, introduces a reproducible regression, or exposes a public-contract conflict. |
| Replace `receive`'s slice argument with `Bytes` | Rejected: forces existing slice callers to allocate before authentication/deduplication or migrate API. The additive entry point closes that current compatibility need. | A separate accepted breaking API change may remove the borrowed entry point. |
| Stream Base64 with `base64` 0.23.1 `Base64Display` and `serde_json` 1.0.151 `Serializer::collect_str` | Supported extension points, but not selected. Resolved `serde_with` currently calls `Engine::encode` to produce a temporary String. Removing it is promising; chunked writes also alter JSON Vec growth from `to_string`'s initial capacity 128, so net allocated-byte savings are not established. | Separate matched evidence justifies preparation work after this outcome or the selected mechanism is reopened. |
| Payload-capacity hint on `JobKind`, reserved `Vec`, `serde_json::to_writer` | Rejected for this candidate: expands a shared public contract and serialization path to eliminate a cost removable at its existing body owner. Needs overflow, estimate, and bound policy plus proof for all kinds. | Measured preparation cost warrants that broader mechanism under a reopened design. |
| Borrowed job payload type or borrowed `Incoming` | Rejected: the existing `JobKind` requires `DeserializeOwned + 'static`; changing it or adding a second enqueue surface is unnecessary. | An independently accepted producer/consumer type separation. |
| Pool, cache, reusable scratch buffer, custom Base64 implementation, new dependency | Rejected: none is needed to transfer the existing buffer; extra lifetime, retention, compatibility, and maintenance policy is unjustified. | A new accepted constraint and evidence unavailable here. |

The slice entry point remains a supported API, not a divergent legacy
implementation: its only difference is when borrowed input becomes owned.
There is no deprecation/removal promise in this change. Remove that entry point
only under a separately accepted API break; keep all behavioral policy in the
single core. Each new private construct is necessary to retain lazy copying
for slices while allowing ownership transfer from HTTP.

## Material flow, authority, and failure

1. HTTP endpoint lookup and `Limited` body collection run in their existing
   order. Inert/unknown endpoints still reject before reading the body; the
   same body/read failure limits and Problem mapping apply. The collected
   handle is moved into `receive_bytes`, whose core borrows the original bytes
   for the existing verifier. The verifier still returns its own `Bytes`
   message identity; its contract, timestamp, algorithm, and errors do not change.
2. The core retains current message-ID validation and content-type capture,
   then enters the same `in_tx_with(READ_COMMITTED)` closure. Receipt insert,
   endpoint/message identity, SQL observation, and transaction ownership stay
   with their existing owners. A duplicate returns without `Incoming::new` or
   enqueue/prepare. Rejected input has no new durable effect or body copy.
3. A receipt winner consumes the private body carrier into `Incoming`. Its
   fields serialize through the existing Base64 adapter into the unchanged
   `prepare` String. Kind/key/delay/serialization/size/decoded-NUL validation
   order is unchanged; no early size estimate or failure mapping is added.
   Enqueue inserts the existing job kind in the same transaction. Concurrent
   identical IDs with different authenticated bodies still retain only the
   first receipt and its corresponding payload.
4. Existing known-failure rollback, cancellation, and uncertain-commit behavior
   remain canonical. Ownership transfer neither retries nor reports durable
   success. The core preserves the current warning/rejection events and error
   classifications; HTTP still returns the existing status and body. A caller
   retry goes through receipt arbitration; it never replays the transaction
   closure automatically.
5. The owned buffer lives in the existing receive future and winning payload,
   then drops through ordinary Rust ownership on success, duplicate, rejection,
   failure, or cancellation. No global retention, new task, queue, pool, or
   cache is created. Moving a handle keeps the existing HTTP collection's
   backing capacity and does not increase it. The borrowed path's one copy
   remains after arbitration. Worker deserialization moves the adapter's
   decoded Vec into `Bytes`; consumers still obtain `&[u8]`. `Incoming::clone`
   may share immutable body storage but preserves its public value behavior.

The HTTP body/admission/deadline limits and 262,144-byte serialized payload
limit remain in their existing owners. This change does not newly promise that
arbitrary caller-created `Bytes` has capacity equal to length; the caller owns
its original allocation, and the receiver never retains it beyond the existing
operation/payload lifetime. Do not compact, slice, normalize, or re-encode the
signed body before verification.

## Responsibility and inverse file map

Placement is mechanically fixed by the existing HTTP and inbound owners in
[Component Boundaries](../../docs/architecture/boundaries.md) and
[Project Structure](../../docs/project-structure-and-module-organization.md).
No owner moves, dependency edge changes, or generated/manual fork survives.
One combined Technical Design review covers this unchanged ownership map;
there is no separate ownership panel.

| Responsibility | Affected path and current evidence | Semantic owner and exact file action | Boundary / cleanup | Proof owner / reopen |
| --- | --- | --- | --- | --- |
| Transfer collected HTTP body | HTTP receive currently lends collected `Bytes` at its receiver call | `infra-http`, `src/webhooks.rs`: move body into additive owned entry point | Same route/state/provider edge; no OpenAPI or middleware change; remove the former lending call | Existing HTTP admission proof; reopen if mapping or bound changes |
| Lazy body ownership and durable payload representation | Receiver inserts before `Incoming::new`; `Incoming` exposes slices and uses Base64 | `infra-webhooks`, `src/inbound.rs`: additive public owned entry point, private shared receive core/carrier, body `Bytes`, constructor consumes carrier | Same verifier/jobs/PostgreSQL dependencies; one core replaces old inline admission body; no duplicate SQL/validation implementation; constructor copies slices only on winner | Adjacent payload parity proof and existing real-DB inbound proof; reopen for API, lifetime, or decoder conflict |
| Admission parity evidence | Existing webhook integration file proves replay, concurrency, rollback, commit-unknown and mounted HTTP | `integration-tests`, `test/tests/webhooks/inbound.rs`: reuse existing proof; adapt/extend only uncovered owned-versus-borrowed parity | Test-only changes under existing profile; no production seam or new harness | Implementation chooses cases and remote commands; reopen if existing proof cannot exercise the owned path |

| Path | Responsibilities / present reason | Declarations and call-path role | Lifecycle / error ownership | Allowed dependencies / forbidden responsibilities |
| --- | --- | --- | --- | --- |
| `crates/infra-http/src/webhooks.rs` | Transfer collected HTTP body | Existing private handler calls public `Receiver::receive_bytes`; existing tests remain local to owner | Existing body collection, HTTP Problem and metrics owners | Existing imports only; no receipt SQL, Base64 policy, or new limit |
| `crates/infra-webhooks/src/inbound.rs` | Lazy body ownership and durable payload representation | Public `receive_bytes` matches `receive` arguments/result except owned body; private receive core and private carrier; private `Incoming.body: Bytes`; public accessors/consumer contracts unchanged | Existing transaction, rejection, deduplication and payload lifetime owners | Existing `bytes`, Serde, protocol/jobs/PostgreSQL imports; no unsafe, custom encoding, config, new public body abstraction, or second admission path |
| `test/tests/webhooks/inbound.rs` if an existing proof gap requires edits | Admission parity evidence | Existing black-box integration-test functions; executor chooses exact cases | Existing isolated database/commit-proxy fixtures | Existing test dependencies; no new service runtime or production-only test seam |

Selected reuse rung: installed library ownership container plus existing
application admission path (`bytes` 1.12.1; `serde_with` 3.23.0). Strongest
rejected source is supported streaming Base64 above. Parity is full JSON-byte
and decoded-value compatibility, plus current rejection/durable observations.
Upgrade condition: no dependency upgrade is selected; changed resolved APIs or
an unmet accepted outcome reopen this design rather than silently adding one.

## Proof map and continuation

| Required claim | Proving surface and boundary |
| --- | --- |
| Same original and durable bytes | Adjacent `Incoming` serialization/deserialization proof compares complete pre-normalization JSON with the fixed Vec-backed baseline or literal vectors; decoded body, metadata, version tolerance and Base64 padding remain compatible. Cover binary bytes and padding edges as warranted by existing coverage. A decoded-value-only comparison is insufficient for the byte contract. |
| Same borrowed and owned admission behavior | Existing real PostgreSQL webhook integration owner: first-wins/replay, simultaneous delivery IDs, enqueue rollback, uncertain acknowledgement/retry, verifier rejection, HTTP limits and mounted response behavior. Exercise the new owned path as well as retained borrowed callers; reuse adequate existing cases. |
| One body-length less construction/preparation turnover | Fresh matched instrumented runs on the approved droplet, at the unchanged combined `Incoming::new` and `enqueue::prepare` boundaries for new deliveries. Include any replacement/helper allocation in that scope rather than moving it outside measurement. Same payload, allocator, feature mode, warmup and identities; report actual body length and allocation counts separately. Verify that the HTTP route reaches the moved-buffer path. |
| No reproducible service regression | Ordinary release comparisons for new small/large deliveries, duplicate-only and retained mixed workloads as specified in [spec.md](spec.md); CPU, useful throughput, latency, errors/drops and peak RSS with repetition/spread. No predicted end-to-end latency percentage becomes acceptance evidence. |
| Build and required behavior proof | Implementation selects repository targets under [Validation Routing](../../docs/validation-routing.md), [Rust validation](../../docs/validation/rust.md), and [PostgreSQL validation](../../docs/validation/postgres.md). Preserve current locked dependencies and optional profile ownership. |

Implementation chooses tests, cases, and commands; no test implementation or
separate test-design phase is part of this result. Static read/review is the
only proof produced here. No local cargo, make, Docker, tests, services, load,
hotpath, Python, or Node analysis is authorized. All required execution belongs
on root-approved DigitalOcean droplet `606044304` (`159.89.101.4`), within
Intent's 8-hour cost/resource cap. Root owns infrastructure and baseline/final
snapshot custody; do not mutate providers or use production inputs.

Planning should preserve this as one bounded implementation outcome followed
by the required remote validation/comparison and final independent review.
No deployment, push, or PR is selected. If the measured allocation target or
regression constraints fail, reopen Technical Design for the smallest repair
or mechanism decision; do not silently add streaming, capacity hints, or pool
tuning. A changed outcome or primary target reopens Definition. Missing host
capability or external authority returns to root. No user-owned decision is
open.
