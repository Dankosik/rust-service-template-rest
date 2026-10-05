# Buffer and resource bounds: selected technical design

Status: ready after Technical Design Review. Source baseline:
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, branch
`codex/buffer-resource-bounds-20261005`.

Authority: [ready specification](../spec.md), [Definition transition](../definition-transition.md)
and [audit dispositions](../research/dispositions.md). The [ownership map](ownership.md)
is part of this fixed candidate. This is design evidence, not implementation,
allocation measurement, runtime proof or delivery acceptance.

## Decisions and preserved boundaries

Use the existing adapters and supported library primitives. No new application
crate, cross-profile utility, resource manager, business route, schema, codec or
configuration key is needed. S1 changes a collection reservation; S2 composes
Smithy interceptors with the already declared body limiter; S3 adds two small
private writer policies to their independent optional owners; S4 adds immediate
admission and cancellation retirement to the current Redis supervisor; S5 uses
native JetStream admission after a narrow same-version native lifetime repair.

The NATS repair is a source change to the already resolved dependency, not a
version upgrade. It is necessary because stock 0.50.0 loses native ACK ownership
after a polled future is dropped and retains unanswered request registrations.
It preserves native transport, ACK parsing, retries/reconnection and automatic
reuse; ordinary cancellation does not permanently retire the messaging resource.

These are byte/count/lifetime bounds. Incoming frame backing allocations,
allocator rounding, decoded objects, caller-owned results and already prepared
events remain outside them. The specification's existing numeric policies remain
the authority; no measured throughput or hard RSS claim is made.

## S1: unread download collection

`Download` already owns `remaining` data and an optional final `last` chunk held
while EOF/checksum completion is pending. `bytes()` reserves from
`remaining + last.len()`, using the same checked representability as the existing
object admission, and consumes the existing `next_chunk()` path. An exhausted
tail requests zero capacity. This does not coalesce, copy or shrink ordinary
streamed chunks, change metadata, or release admission before actual completion.

The material trace is caller-held Download -> unread-tail reservation -> existing
chunk/EOF state machine -> returned Bytes. Cancelled polling can leave `last`
present with `remaining == 0`; its bytes belong to the resumed collection. Failure
still clears held data and repeats the same error. Returned Bytes outlive the
download permit, as before. Proof belongs beside `download.rs` and the existing
HTTP stub: unread, partial, exhausted and held-final-chunk states expose the
requested capacity and returned bytes without a memory benchmark.

## S2: SDK collection envelope before parsing

The revised Definition limits actual DATA of all current nonstreaming responses:
PUT, HEAD, DELETE and HeadBucket, plus GET non-success replies. In the resolved
SDK, control operations collect first and can discover XML Error inside 2xx only
afterward. Successful GET remains entirely on its current streaming path.
HEAD Content-Length remains object metadata and is never compared to this cap.

Use a private response-envelope interceptor with two nonoverlapping placements:

1. One client-level instance handles non-2xx replies for every dispatched call.
2. One operation-level instance handles 2xx replies on PUT, HEAD, DELETE and
   HeadBucket. The existing mutation `config_override(once)` is retained; GET and
   local presigning receive no success-body interceptor.

Both instances use the same body composition: take the response SdkBody, map
non-DATA frames to empty DATA, wrap that body in `http_body_util::Limited` with
1,048,576 bytes, and replace it through `SdkBody::from_body_1_x`. Original DATA,
pending, EOF and errors pass through the native combinators. The SDK collector
already discards trailers after collection; discarding them before collection
prevents an unbounded retained trailer map. Empty DATA adds no retained list
entry. There is no complete-body copy or synchronous special case.

Placement is consequential. Client interceptors run before generated operation
interceptors. GET's generated checksum wrapper therefore sees the data-only
limited error body, avoiding its assumption that every successful frame is DATA.
Successful GET never enters either envelope, preserving the existing checksum,
EOF and final-chunk path. None of the four current control operations installs a
response-checksum interceptor in this SDK version. The split avoids a new direct
Smithy dependency merely to name operation Metadata, and does not rely on ordering
among user interceptors at the same level.

At exactly the ceiling the wrapper still polls to actual EOF; any additional DATA
byte raises the native body-limit error. Content-Length, size hints and missing
lengths cannot bypass actual-frame counting. This bounds bytes exposed by the HTTP
body implementation; HTTP framing and a single incoming frame's backing allocation
remain native transport concerns.

A wrapper error enters the SDK's response-error path, then existing `Reply::Lost`:
read/probe Unavailable, mutation/create-only OutcomeUnknown. It is never object
TooLarge or a new definite mutation rejection. Returning an interceptor error
directly would have a different SDK retry classification and is rejected. Normal
XML/status/code/request-ID parsing remains SDK-owned, existing read retries remain,
and mutation overrides still allow one attempt only. The native limiter replaces
an unnecessary custom collector; a status-only limiter was rejected because it
misses 2xx embedded errors. Reopen this mechanism on SDK hook-order or operation
changes. Existing in-process HTTP/body fixtures can falsify framing, late errors,
ordinary provider errors and successful checked streaming; no live bucket is needed.

## S3: one serialization, bounded retained output

Use `serde_json::to_writer` once, with a private `std::io::Write` implementation
at each current preparation owner. The writer retains at most that owner's
ceiling, counts all encoded bytes, and reports each write consumed even after
the retained prefix is full. It must not return a size error from `write` or
reserve from serde input hints. Capacity requests grow geometrically only up to
the ceiling; no ceiling-sized eager reservation for small payloads is needed.
Counting uses checked representable arithmetic, never wraparound.

Only after serialization succeeds may preparation inspect the exact count. Jobs
return the existing `PayloadTooLarge { bytes }` before the existing decoded-NUL
check; successful bytes become String without another serialization. Messaging
returns its current oversized Envelope reason before ID/time validation. A late
serializer failure retains the existing Serialize/Envelope error even after
overflowing the retained prefix. Under-limit bytes use the same compact serde_json
serializer and remain identical. Custom serializers' own allocations and CPU
remain caller-owned.

Both enqueue and `compare_live_payload` already call jobs' private `prepare`, so
one replacement closes both paths before SQL. Outbox preparation continues using
the same event bytes and its existing padded-base64-plus-metadata accounting;
there is no new stored format or second serializer pass.

The two tiny private writer policies deliberately stay in `enqueue.rs` and
`prepared.rs`. Jobs and messaging are independently removable; sharing through
jobs would break messaging-only, a new utility crate would add no ownership
boundary, and moving wire policy into domain-events would violate its boundary.
`Vec` alone is unbounded, a fixed slice/early-error writer loses late-error
precedence, and two-pass counting can invoke a stateful serializer twice. The
selected std writer policy is the smallest uncovered mechanism. Reopen sharing
only if a real common owner already required by both profiles emerges. Proof is
local preparation behavior, not a new database statement or migration claim.

## S4: immediate cache admission and generation retirement

`Link` owns a Tokio semaphore of 256 application operations. `command()` uses
`try_acquire` before waiting for a generation, so disconnected waiting callers are
also finite; capacity refusal returns existing ErrorType::Other through sanitized
Unavailable and does not retire a healthy generation. This resource-level bound
is sufficient for the per-generation 256 limit and avoids a new admission queue.
The permit spans acquisition and the unchanged absolute command deadline.

After selecting a generation, a private exchange guard owns that generation and
the application permit. It is armed immediately before polling the native query.
On caller drop, timeout or failed exchange it synchronously calls existing
`Shared::retire` before releasing capacity. On a successful response it disarms.
Cancellation while only acquiring a generation releases admission without
retirement. Retirement's generation-identity check cannot withdraw a successor.
Admitted peers stop through the existing retired token; the current supervisor
alone reconnects with its existing backoff, credentials and publication spacing.
No operation is replayed. SET/DEL cancellation still proves neither effect nor
absence, and the cancelled observation remains the existing one.

External probes use one separate immediate permit on Link, spanning acquisition
and the same cancellation-safe exchange; excess probes return Unavailable.
The existing supervisor has at most one PING/AUTH exchange at a time and remains
outside application admission. It continues using its current raw exchange owner,
including the current rejected-AUTH handling. Thus at most 256 application,
one external probe and one supervisor exchange can own command work for this
resource; setup is the separately bounded existing supervisor phase. Maintenance
cannot queue behind an adapter application permit or spawn accumulated work.

Redis's native concurrency control was considered first: it waits instead of
refusing immediately, and its permit belongs to the cancellable caller future.
It cannot provide either required property alone; adding it beside the selected
gate adds no necessary bound and can obstruct maintenance. Keep native pipeline
50 and write-flush threshold 8 KiB unchanged. These are entries and a soft flush
threshold, not command/value/decoder byte caps. Existing generation retention and
owner-drop/driver-abort behavior remain; no new task, connection or timeout is added.

Existing silent-server/recovery fixtures and adjacent connection tests can observe
dispatch, cancellation, peer failure, new-generation recovery and no write replay.
The proof must distinguish capacity refusal from post-poll generation retirement.

## S5: repaired native publication ownership

### Native source selection

Use a root Cargo patch for published `async-nats 0.50.0`, excluded from the
workspace, following the existing `vendor/sqlx-core/PATCHES.md` custody pattern.
Published archive:
[async-nats-0.50.0.crate](https://static.crates.io/crates/async-nats/async-nats-0.50.0.crate).
Cargo.lock checksum is
`d83a251fa1a4c9d0fe6e816b7acd60549e473e08d14f27a1d992c2675abff05f`;
published VCS revision is `9b382a2a01b5404cd66bee6c2b4f0c82c9943063`.
Registry/latest evidence on 2026-10-05 still identifies 0.50.0, released 2026-07-20.

Upstream [issue 1617](https://github.com/nats-io/nats.rs/issues/1617) confirms the
unanswered-request lifetime defect; [PR 1618](https://github.com/nats-io/nats.rs/pull/1618)
was open and unmerged at inspected head
`d1c31fde9a6e7b3c9bc4d6331f3b59b6ef376f26`. It is provenance and an alternative,
not a released fix or an accepted patch to copy wholesale. Its best-effort
DiscardRespond path can spawn a waiting task when the command channel is full;
that fallback does not satisfy this task's bounded cleanup requirement.

A later source check found [upstream draft PR 1629](https://github.com/nats-io/nats.rs/pull/1629)
at inspected head `7db17cf15830a1a65e7ba73cecda65aee72b1ea7`, also open and
unmerged on 2026-10-05. Select its handler-local adaptive pruning as the smaller
cleanup mechanism, plus the independently necessary ACK-lifetime repair. The
initial dirty-flag/receiver-wrapper alternative required Client/handler plumbing
and a scan after each abandonment; it is superseded before Implementation.

The selected repair changes only `src/lib.rs` and `src/jetstream/context.rs`:

- The native multiplexer keeps a pruning threshold, with the upstream metadata
  floor of 256 entries. Before insertion reaches that threshold it removes closed
  senders, then sets the next threshold to the larger of 256 and twice the retained
  live count. Reply removal lowers the threshold as a burst drains. Excess map
  capacity can shrink at the upstream threshold. This amortizes sweeps without a
  new receiver type, shared atomic, cleanup command, channel or spawned task.
- The floor is an internal stale-metadata reserve, not an application publication
  limit, response-size policy or queue. A cancelled queued request may insert an
  already-closed sender, but the same threshold prunes it; repeated waves cannot
  accumulate unanswered history. Preserve current reply subjects and publication
  enqueue even when the receiver has gone away.
- The polled `PublishAckFuture` awaits a mutable borrow of its retained receiver.
  Cancellation therefore leaves the receiver in Self, and existing Drop moves
  receiver plus permit to the bounded native acker. A completed ACK or native
  timeout removes/drops the receiver before returning or applying error
  propagation, so timeout does not schedule another full ACK wait.
- Receiver closure precedes permit release on native timeout and acker expiry.
  Native parsing, error kinds, ACK fields, expected-stream header behavior and
  connection/reconnect machinery stay intact. The adaptive cleanup uses ordinary
  oneshot closure from all native request callers; `client.rs` is unchanged.

Reclamation happens on subsequent insertions or handler destruction, so an idle
expired cohort can leave finite stale metadata. For a client's peak simultaneous
live request count L, the map-entry ceiling is the larger of 256 and twice L;
L includes native publication permits (at most P) plus separately owned live
control requests. This is not a claim that every map entry holds a payload, that
all metadata expires at the caller deadline, or that the allocation equals the
entry count. The current source-settlement/admin/durable-info paths receive the
same cleanup, so the repair is not publication-only. Ordinary reconnection and
healthy reuse remain unchanged.

The upstream handler algorithm is the selected source, with its identity and
production delta retained in PATCHES.md; no unrelated PR changes are selected.
Its tests are useful evidence inputs, but Implementation chooses the repository's
necessary regressions and runner under the existing validation owner. Pairing it
with ACK retention is required: handler pruning alone cannot retain the publication
permit after a caller drops an already-polled ACK future.

### Admission, wire checks and cancellation

Build the one shared JetStream Context with
`max_ack_inflight(P).backpressure_on_inflight(false)`, where
`P = floor(67_108_864 / (M + 8192))`. Existing validated M makes P positive;
the adapter's public direct-construction bounds must reject invalid arithmetic
through existing MessagingError::Bounds. Every source/outbox/DLQ helper keeps
using that context; clones share its semaphore, and no second context is built.
`MaxAckPending` already maps to pre-dispatch Rejected. There is no adapter queue
or duplicate semaphore.

The source/outbox receiving boundary checks payload <= its own M and final headers
<= 8192. Final means both trace injection and `Nats-Expected-Stream` are already
included. Build a native PublishMessage only after checking that same HeaderMap;
the common send helper must not add another header afterward. DLQ prepares its
expected-stream header through the same native header vocabulary but skips the
ordinary M/H check. Its copied malformed-source bytes and stable restore framing
remain intact. Native `send_publish` checks full headers+payload against advertised
broker max_payload before acquiring a permit or queueing a command.

After admission the existing minimum of caller deadline and five-second broker
budget still bounds the caller's wait. Possible-dispatch cancellation returns
Ambiguous. The repaired native acker retains capacity after caller drop until
ACK or its existing five-second cleanup timeout. Queued plus running acker work
holds at most P native permits; its existing unlimited cleanup concurrency means
timers are not serially queued. Under a progressing executor, cancellation near
the caller deadline can retain a slot about five seconds longer, approximately
ten seconds from initial admission. This is a resource-lifetime bound, not an
extension of the caller deadline. After expiry native capacity is reusable and
later request insertions prune stale correlation within the stated metadata ceiling;
no resource restart is required.

An exhausted window rejects a DLQ transfer before dispatch, so its existing
failed-transfer/redelivery path retains the source. A confirmed DLQ ACK still
precedes source ACK. No publication admission permit is required merely to send
source settlement/control traffic, so all permits occupied cannot prevent
settlement or create an admission deadlock.

### Separate native reserves

| Reserve | Unit and lifetime outside the ordinary 64 MiB wire target |
| --- | --- |
| Source/outbox active publication | At most P payloads of M plus final headers of H; caller-owned prepared backing remains separate. |
| DLQ shared publication window | Same P; conservative wire ceiling is P times broker-advertised max_payload, because malformed source data can exceed M. |
| Command channel | 2048 command entries until handler consumption or destruction; entries have variable payload size. |
| Handler batch and write buffer | Up to 16 commands per received batch; 65,535-byte soft write threshold is checked before a batch, so allow a batch of protocol/payload overshoot and control framing. |
| Subscription buffers | 65,536 messages per ordinary subscriber, until delivery/drop; ACK mux uses oneshot receivers instead. This is count, not a 64 MiB allocation claim. |
| Native acker | Queue capacity P and at most P queued plus running receivers/permits; existing five-second expiry once polled. |
| Request registrations | At most max(256, 2 × L) entries for peak live request count L, including P publication permits and separately owned control calls. Idle stale metadata may remain; upstream adaptive pruning/shrink prevents cumulative abandoned history. |

The alternative persistent inbox would need a new bounded map, driver, shutdown
ownership and ACK/error control flow. Per-call explicit inboxes additionally spawn
unjoinable subscriber-drop work. Automatic client replacement must own old/new
generations and rebind consumers, while public Client exposes no hard abort/join
and reconnect can delay drain. Permanent failure on ordinary caller cancellation
is an unnecessary recovery regression. The selected vendor repair is smaller,
repairs all current mux callers, and keeps native protocol behavior. Reopen it when
a maintained release includes equivalent cancellation and request cleanup with
proof; remove vendor, patch, exclusion, image/profile entries together then.

### Dependency and delivery custody

Implementation verifies the published archive checksum before extraction, retains
licenses and normalized manifest, and records exact pristine/patched file hashes,
diff, upstream references and removal condition in vendor/async-nats/PATCHES.md.
Unchanged archive source stays byte-identical. No dependency version or feature
change is selected. Cargo.lock changes deliberately record the path source while
preserving version/dependency identity; use Cargo's supported resolution and
verify the semantic delta, never incidental validation regeneration.

The messaging profile owns vendor removal, Cargo patch/exclusion and Docker
context/cooked-source markers. The existing classifier must select dependency,
messaging integration, image and initializer surfaces for the vendor. Preserve
locked metadata and retained/messaging-absent profile behavior. Dependency Review,
cargo-deny and existing release/CI gates remain; no advisory waiver or upstream
publication is selected. These selectors route the assembled final validation,
not per-task repeated builds or a new infrastructure matrix.

## Exact-version source evidence

The installed registry source matches Cargo.lock. Paths below are relative to
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` and are read-only
evidence; implementation must use the pinned packages, not another cached version.

| Mechanism | Decisive source anchors |
| --- | --- |
| Smithy hook and ordering | aws-smithy-runtime-api-1.18.0/src/client/interceptors.rs:378; context/wrappers.rs:216; runtime_plugin.rs:65; aws-smithy-runtime-1.15.0/src/client/orchestrator.rs:190 and 508; runtime_components.rs:171 in runtime-api. |
| Body composition | aws-smithy-types-1.8.1/src/body/http_body_1_x.rs:18; http-body-util-0.1.5/src/limited.rs:34, collected.rs:42, combinators/map_frame.rs:52. |
| S3 collection and interception | aws-sdk-s3-1.150.0/src/operation/get_object.rs:328, put_object.rs:408, head_object.rs:273, delete_object.rs:229, head_bucket.rs:209; config.rs:1899; client/customize.rs:51; http_response_checksum.rs:100. |
| JSON | serde_json 1.0.151, std feature, [to_writer API](https://docs.rs/serde_json/1.0.151/serde_json/fn.to_writer.html), compact serializer; jobs and messaging already declare this feature. |
| Redis limits | redis-1.7.1/src/client.rs:315–374 and aio/multiplexed_connection.rs:773–816: native admission waits and its guard is caller-owned; pipeline/write defaults remain. |
| NATS native admission and gap | async-nats-0.50.0/src/jetstream/context.rs:180, 249, 319, 483, 1865–1923; lib.rs:783, 889, 1012; client.rs:721; only the two named native request construction sites. |
| NATS wire/reserves | async-nats-0.50.0/src/client.rs:369; header.rs:159; jetstream/message.rs:123; options.rs:112; connection.rs:45; lib.rs:514 and 600. |
| Cancellation primitives | tokio-1.53.1/src/sync/mpsc/bounded.rs:816 (reserve then synchronous send); sync/oneshot.rs:773 and 947 (closed observation/close). |

Versioned docs.rs availability was mixed; installed resolved source supplies the
API conclusions where the web reader could not open a page. Upstream NATS issue/PR
and current source were independently read; their existence is not runtime proof.

## S6 documentation and proof boundary

Update current object-storage, cache, durable-messaging, background-jobs and outbox
guides with the changed bounds and lifetime qualifications above. HTTP architecture
and existing backend utility recipes explain feature-owned response guards held through body completion/drop,
finite response size and write/lifetime policy for slow clients; it does not install
universal HTTP policy. gRPC guidance uses existing generated client/server maximum
encoding/decoding setters and feature-owned stream/message admission. Authentication
guides only correct count/weight/RSS wording. Existing profile markers remain.

Implementation chooses cases/assertions/commands beside these owners and reuses
current fixtures. Material proving surfaces are capacity at the boundary, exact
serializer error precedence, source EOF/checksum preservation, repeated cancelled
unanswered commands, ACK polling cancellation/expiry/healthy reuse, adaptive pruning after cancellation/bursts
and queued cancellation, final header size, cross-resource prepared events,
and DLQ confirmation before source ACK. The vendor patch needs both native lifetime
and public adapter parity evidence; no new provider provisioning is required.

Final validation follows the current mixed-surface route once on the assembled
candidate: matching build/workspace tests for manifest/multiple-crate changes,
selected static/dependency/documentation checks and existing CI-owned optional
integrations/image/initializer gates. No task-local mandatory full build, benchmark,
live provider call or full-repository check is introduced. Independent assembled
delivery review remains required. Any implementation evidence contradicting a
mechanism returns to this design owner; behavior changes return to Definition.
