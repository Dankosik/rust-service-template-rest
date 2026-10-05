# Buffer and resource bounds specification

Status: ready. Baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Authority: [Intent](intent.md). Evidence and full audit dispositions:
[Research](research/dispositions.md). No user-owned decision remains open.

## Outcome and boundary

Close avoidable unbounded collection or queued work in existing provider adapters,
without changing successful wire payloads or inventing feature behavior. Required
implementation surfaces are S3 remaining-body and nonstreaming-response collection,
job/event JSON preparation, Redis outstanding work, and publication admission.
Documentation states the scope and lifetime of each budget. All numeric choices
below are local template policy targets, not protocol limits or measured capacity.
Technical Design owns mechanism and placement; it may reopen Definition if a
selected target cannot preserve the stated behavior.

## S1. Collect only an unread object tail

`Download::bytes()` returns only bytes not already returned by `next_chunk()`.
Its collection reservation must reflect the unread payload including any final
chunk held pending EOF, rather than the original object's size. A completed empty
tail must not reserve an object-sized allocation. The payload cap remains
`max_object_bytes`; allocator rounding and SDK buffers are outside that cap.
A cancelled `next_chunk()` that has retained the final chunk must not lose it when
`bytes()` resumes. Preserve stable repeated errors, permit release at terminal
completion/drop, and the final-chunk hold until EOF and supported checksum check.
Nearest falsifier: partially read and exhausted downloads plus a pending final
chunk, observing collected bytes and requested collection capacity.

## S2. Bound SDK nonstreaming-response collection

Every response from the currently used nonstreaming S3 operations (PutObject,
HeadObject, DeleteObject and HeadBucket), plus GetObject error responses, has a
1 MiB encoded-body ceiling before SDK collection/deserialization. This includes
nonstreaming 2xx replies: PUT/DELETE can contain an XML Error even when their HTTP
status indicates success, and the SDK reads the body before deciding. An HTTP
status-only limit is insufficient. Successful GetObject remains streaming under
`max_object_bytes` and the existing EOF/checksum contract; the 1 MiB envelope does
not cap its object body.

The nonstreaming ceiling is independent of object size and enforced during reading
even without or with a dishonest Content-Length. It follows the template's existing
1 MiB provider-response scale: control/error replies need no object-sized allowance.
This is local defensive policy, not an S3 specification maximum. At the ceiling a
complete body is parsed unchanged; an additional data byte fails the response.
Do not present truncated XML as complete, or buffer the full response before
checking. Empty data frames and metadata must not create an unbounded retained
frame list. Keep ordinary SDK success/error parsing (including Error XML under
2xx), code/status mapping, request identifiers, signing and checksums intact.

An unreadable or over-limit nonstreaming response uses existing lost-response
semantics regardless of its HTTP status: `Unavailable` for a read, `OutcomeUnknown`
for a mutation/create-only put. It is never object `TooLarge`, success, a definite
mutation refusal, or proof that a mutation did not occur. Existing read retries
and one-attempt mutations remain unchanged; no new retry is added.
Nearest falsifier: fragmented/chunked and misleading-length nonstreaming replies
at and over the ceiling for both 2xx and error statuses, ordinary named provider
errors including 2xx Error XML, and a successful checked GET larger than 1 MiB.

## S3. Bound JSON preparation without changing accepted JSON or errors

Jobs (including `compare_live_payload`) and `PreparedEvent::prepare` must serialize
once while retaining at most their existing serialized payload ceiling. The sink
must not reserve from an input's untrusted size hint or grow to the oversized
output size. Successful output stays byte-for-byte equivalent to the existing
serde_json output and follows the existing validation order.

For jobs the ceiling is `MAX_PAYLOAD_BYTES` (262144 bytes). Fully serializable
oversized input still returns `PayloadTooLarge { bytes }` with its exact total
encoded length; serialization failure still wins over the later size and NUL
checks, including a serializer that fails after emitting more than the ceiling.
For messaging retain the same Envelope reasons and precedence: a serialization
failure remains `event payload cannot be serialized`; only successfully serialized
oversized input is `payload exceeds configured maximum`. Therefore discarding
excess output while counting it is allowed; aborting at the first excess byte and
misreporting a later serialization error is not. Retention is bounded, not CPU
spent inside an application-defined serializer or allocations it makes itself.
No database/broker effect occurs on preparation refusal. Outbox encoding must
continue accounting for padded base64 plus metadata inside the jobs ceiling.
Nearest falsifier: exact boundary, much larger output, late serialization error,
and exact byte count with unchanged accepted payloads/validation precedence.

## S4. Bound Redis outstanding work across caller cancellation

Each connection generation admits at most 256 application commands that may be
awaiting a response. This is a local resource policy aligned with the template's
256 default HTTP handlers, independently enforced for non-HTTP callers too.
Finite native pipeline (50 entries) and write-flush (8 KiB threshold) defaults
remain adequate; neither is a byte cap on one command or response.

Capacity refusal occurs before dispatch and returns the existing sanitized
`CacheError::Unavailable`; do not add an unbounded adapter admission queue. All
admitted work remains within the existing command deadline. A caller dropping a
command after possible dispatch must not free capacity for unlimited unanswered
commands on a still-usable generation. Retain ownership until response cleanup,
or retire that generation before reusing its capacity. Cancellation still drops
the caller promptly and never proves that SET/DEL did not execute. If retirement
is selected, other commands on that generation may fail as Unavailable and normal
supervisor recovery applies; do not replay writes. Pre-dispatch cancellation need
not retire a healthy connection. Maintenance (PING/AUTH) remains separately finite
and must not accumulate or lose its recovery function under application load.

The count is not a key/value byte ceiling, nor a hard process memory limit. Caller
inputs/results and feature fan-out remain caller-owned. Preserve current credential
rotation, redaction, bounded generations/reconnect lifecycle, readiness degradation
and at-most-once dispatch. Native redis controls are preferred, but their guard's
release on caller drop alone is insufficient evidence for this requirement.
Nearest falsifier: a silent server with excessive concurrent and repeatedly
cancelled commands; sent-but-unanswered commands cannot grow without bound on a
usable generation, recovery remains possible, and no write is replayed.

## S5. Give publication its own finite admission

One messaging resource has a separate finite publication window. For its source
producer and outbox, let M be the resource's configured payload ceiling and H the
existing 8192-byte header allowance. Require payload <= M and final encoded headers
(including injected tracing) <= H at the receiving publication boundary, regardless
of the limit used to prepare or restore that event. Excess input returns existing
`PublishError::Rejected` before dispatch. Accepted JSON, IDs and headers are not
truncated or rewritten to fit. This closes the cross-resource prepared-event bypass.

The common simultaneous-publication count is
`P = floor(64 MiB / (M + H))`; valid existing M guarantees P >= 1. Thus ordinary
source/outbox publication has a 64 MiB wire-window target. This reuses the existing
consumer scale as a separate local target; the two windows add and are not RSS.

DLQ transfer preserves its current malformed-source behavior: source payload and
restorable headers are copied as today, including a source payload larger than M
that still fitted the admitted source-stream wire limit. Do not apply the ordinary
producer M/H check to this internal transfer, truncate it, or ACK its source on a
new local size refusal. DLQ shares the count P, but its wire-size ceiling remains
the connected broker's advertised `max_payload`, which the native client checks
before enqueueing, plus the existing DLQ stream's acceptance policy. Therefore
the conservative common wire window is P times that advertised maximum when DLQ
is present, not 64 MiB. Document this larger, operator-dependent bound separately;
the source consumer's existing window still bounds retained source deliveries.
A DLQ rejected by the native broker-size check or stream policy retains the source
and follows existing failed-transfer redelivery. No topology is changed to hide a
size incompatibility.

Use existing native admission where it covers the whole retained work. Excess
publication is refused without dispatch as existing `PublishError::Rejected`,
rather than queuing indefinitely. The source producer, outbox publication and
DLQ publication obey the same resource bound. Settlement must remain possible:
capacity shortage cannot cause source ACK before DLQ confirmation or a deadlock;
it follows the existing failed-DLQ/redelivery path. An admitted publication keeps
its current deadline and cancellation behavior, preserving `Ambiguous` after
possible dispatch, expected-stream ACK validation, and stable IDs/replay framing.
Cancellation/drop must not defeat the declared outstanding-work bound; retain
native ownership through ACK cleanup or bounded retirement/expiry as appropriate.

Native command/subscription/write buffers are separate finite reserves. Explicitly
account for their count/byte unit and lifetime in the guide; do not claim the
64 MiB target includes them. Prepared-event clones share backing and already
prepared caller-owned events remain outside publication admission. Producer
admission cannot make arbitrary caller preparation fan-out bounded.
Nearest falsifier: occupied publication capacity, an excess call that is conclusively
refused before dispatch, and cancellation/DLQ settlement that preserve ownership
and ambiguous outcomes without accumulating unbounded adapter work.

## S6. Correct and complete the adopter-facing explanation

Update the existing guides/examples only where necessary to explain these fixes
and [all audit dispositions](research/dispositions.md). State actual boundaries:
payload length versus backing capacity, active operation versus returned-result
lifetime, wire versus decoded allocations, and count/weight targets versus RSS.
Explain a feature-owned admission guard held through response body completion/drop,
paired with finite payload and response-write/lifetime policy for slow clients;
use an example or recipe rather than installing universal HTTP policy. Presigned
URLs remain an option for objects too large for a feature's buffered-response
budget. A promptly read stream is not a promise that an unpolled reader will end.

For new gRPC business services, show supported per-service/client send and receive
message-size configuration and feature-owned aggregate stream/message-count and
caller-lifetime policy. Keep the template's current protocol and listener behavior.
For cache features, state key/value/decoded-size and concurrent-call ownership and
server maxmemory/eviction requirements; response-size checking after GET cannot
retroactively bound Redis decoder allocation.

## Deliberately unchanged and non-goals

HTTP's 1 MiB request limit, 256 default handler permits and 8 s handler timeout
still end their protection at the appropriate existing boundary; no response-body
timeout or universal response-size cap is introduced. Existing outbound HTTP
sequential collection, deadlines and deliberate trailer discard stay unchanged.
Stock prost framing/errors and the 2 KiB initial/32 KiB batch buffers remain;
4 MiB default gRPC decoding and unlimited default encoding are accurately documented,
not replaced by a guessed business-message policy. Compression remains disabled.

Do not shrink/copy every Bytes slice or Vec, replace library codecs, redesign
introspection/token-exchange caches, add a global memory manager, or promise hard
RSS bounds. Consumer settlement ownership, NATS topology authority, outbox
base64/identity, upload ExactLength, EOF/trailer/checksum completion, and typed
mutation ambiguity remain authoritative. No schema, migration or business route
is added. Existing optional profiles remain removable and inert when unselected.

The no-upgrade constraint does not forbid a narrow, evidence-backed source lifetime
repair in the same resolved dependency version when supported native APIs cannot
satisfy an accepted ownership invariant. Such a repair follows the repository's
existing dependency-patch delivery pattern and preserves native admission, ACK
parsing, automatic recovery and public protocol. Technical Design owns its exact
scope, proof and removal condition. This admits the confirmed async-nats 0.50.0
lifetime repair for S5; it does not add a public protocol, task/queue system or
memory manager, or change S5's behavior, bounds and outcomes.

## Proof boundary and next owner

Implementation selects focused cases with the existing unit/in-process fixtures;
the nearest falsifiers above define behavior, not a mandatory infrastructure matrix.
Reuse adequate existing proof. Final validation follows AGENTS.md's changed-surface
budget and preserves applicable CI gates; no provider provisioning, benchmark,
full repository check, or duplicate heavy run is required by this specification.
An independent assembled delivery review is required because S4/S5 affect
concurrency and cancellation safety. Local proof, CI and PR publication are reported
separately. Technical Design next closes supported mechanisms, placement and proof
seams for S1–S5, with explicit cancellation ownership and SDK interception evidence.
