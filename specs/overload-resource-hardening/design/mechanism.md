# Overload hardening technical design

Status: ready. Owner: Technical Design. Fixed source:
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`.

[Specification](../spec.md) owns G1/S1/D1 behavior. Its ready identity and
review are in [Definition result](../definition-result.md); this design changes
no accepted behavior or authority. [Ownership](ownership.md) is the inverse
file/responsibility map. No product source has been edited in this phase.

## Drivers and selection

The two enforcing boundaries remain `infra-grpc` server assembly and
`infra-object-storage` GET/Download. Neither moves into service bootstrap,
feature code, the authentication provider or a common admission crate.

| Decision | Existing/native alternative considered | Selection, cost and reversal evidence |
| --- | --- | --- |
| Pre-authentication opening count | Existing Tokio owned semaphore; Tower concurrency/load-shed layers; moving the existing terminal semaphore outside authentication | Reuse a second independent semaphore with the same configured K in the existing router. Moving the terminal semaphore spends authenticated capacity on unverified openings and merges two distinct lifetimes. Tower can bound a service future, but does not replace the existing gRPC status, bounded rejected-upload drainage, health routing and terminal body custody; adapting it adds glue without deleting these owners. Two semaphores and one small private middleware are the present cost. Reopen only for changed count/lifetime policy. |
| Returned GET lifetime | AWS operation/attempt timeout and stalled-stream protection; HTTP-body wrappers or Tokio timeout around each read | Keep SDK retry/attempt/stall settings, add one original local absolute deadline. SDK operation timeout excludes response ByteStream consumption; a polling wrapper cannot destroy an unpolled body it exclusively owns. No documented SDK interceptor owns returned Download plus permit/observation. Reopen when an SDK/native facility owns all four resources and independently releases them with the same finality. |
| Autonomous custody | Existing `infra-grpc/src/call.rs` response custody; one producer task and bounded channel; a central sweep registry | Specialize the existing Weak-timer/shared-state pattern inside current `download.rs`. Pulls remain caller-driven, with no producer/channel, prefetch, registry, long-lived task or bootstrap lifecycle. Cost is one small state allocation/mutex and one sleeping task per active returned GET, already bounded by storage admission. A producer introduces cancellation, queue and chunk-handoff semantics without need. A registry adds shared coordination and shutdown policy. Reopen if a demonstrated contention/scale constraint invalidates this bounded cost. |
| Poll cooperation | Tokio 1.53.1 `task::coop::poll_proceed`; custom batch constants/yield counters | Use native cooperative budget before each SDK body poll, commit budget on each Ready result (including empty bytes), restore on Pending. This bounds scanning and repeated immediately-ready next_chunk calls in a Tokio task, including bytes(). No workload quota or public tuning key. Runtime callers deliberately disabling cooperation or blocking inside a single poll are outside the accepted functioning-cooperative-runtime guarantee. |

Current declared versions are AWS S3 1.150.0, Smithy types 1.8.1, Tokio 1.53.1,
HTTP-body 1.1.0 and Tower 0.5.x from the fixed lockfile. Native source was read
from the installed Cargo registry: Tokio `runtime/task/join.rs` and
`task/coop/mod.rs`; Smithy `byte_stream.rs` and `body.rs`.
`aws-config/rt-tokio`, already enabled by this crate, enables `tokio/rt`; no
manifest, feature or lockfile change is needed for spawn or cooperative polling.
[AWS timeout documentation](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/timeouts.html)
explicitly excludes returned streaming data from operation/attempt timeouts.
[HTTP-body 1.1.0](https://docs.rs/http-body/1.1.0/http_body/trait.Body.html)
exposes polling and end hints, not an independent cancellation owner. Versioned
Tokio web pages were unavailable, so resolved local crate source, not guessed
latest API behavior, supplies that evidence.

## G1 material flow and ownership

Business inbound order is observation -> existing opening deadline -> new
opening admission -> existing authentication/scope policy (when retained) ->
existing terminal admission -> handler. Health remains registered outside all
business layers. Axum layers are applied inside-out; preserve that actual order.

At router composition construct two separate Arc<Semaphore> values only when
`Limits.max_in_flight` is Some(K). The existing terminal `shed` keeps its current
permit extension/body transfer. The new private opening middleware acquires
with `try_acquire_owned`, holds the local RAII permit while awaiting `next.run`,
and drops it as soon as the response head is returned. It never inserts that
permit into extensions. Router clones share each Arc. None installs neither
admission layer. The opening layer is outside the authn template marker, so a
retained gRPC profile with authn removed still bounds handler openings.

An opening refusal calls existing `record_shed`, `at_capacity` and `reject`
once, before verifier/provider/handler dispatch. It owns no admitted permit.
The existing deadline remains outermost, with its original origin, biased timer
selection and post-response expired check. Thus expiry wins over admission or
provider/handler response at the same observation point and may cut short
rejection drainage; no new deadline is constructed by admission. Terminal
capacity refusal occurs only after successful authentication and returns
through the same existing path. One call cannot fail both admissions.

`reject` retains 100 ms/64 KiB drainage; its existing frame loop must cooperate
on ready frames as well, so an empty-frame upload cannot defeat the existing
time bound. Use the existing Tokio cooperative yield facility in that loop,
without increasing its byte/time limits or claiming a rejection-future cap.
This is part of preserving bounded G1 rejection, not a new workload policy.

Response head, auth refusal, handler error, unwind, cancellation and opening
timeout release the opening permit by scope drop. Existing call body custody
alone releases the authenticated terminal permit. A cancelled auth follower
stops consuming opening capacity even if separately owned provider singleflight
work continues under the verifier's existing deadline/ownership. Health Check
and Watch retain socket/transport controls and no business capacity/deadline.
No auth implementation, public RPC contract or existing call-body mechanism
needs modification.

## S1 absolute deadline and pre-header flow

At the first execution of `ObjectStorage::get`, capture one Tokio absolute end
before starting the observation, admission, SDK preparation or dispatch. Use the
existing saturating `tokio::time::sleep(operation_timeout).deadline()` idiom
already in PUT; do not use unchecked Instant addition. Store that same end in
Download; a handoff/read/retry never resets it.

Admission stays fail-fast. Check expiry before admission outcome is committed,
before the first poll of SDK send, after every send poll that returns Ready,
and before accepting response metadata or handing off the open Download. A
small private pre-header polling boundary in `lib.rs` races the existing send
future against that end and checks the clock before polling the send future:
`timeout_at` alone is insufficient to promise no first dispatch after expiry
if preparation already used the budget. The ready result is checked again
before choosing its existing error/success mapping. Keep SDK standard retries
and half-budget attempt limit; the outside deadline cancels the entire send
future, including retry waiting, at the original end. No additional retry loop
or SDK dispatch is created by the adapter.

Header request IDs stay with the same OperationGuard. Range, length, object
limit and provider failures retain their current mappings if selected before
end; local expiry selects `Unavailable` with existing `timeout` error type.
Expired output is dropped, never handed to the caller. Metadata interpretation
is synchronous bounded header work and has a final expiry check before its
result is committed. Dropping get before handoff drops its SDK future, guard
and permit and records cancellation as today.

Successful nonempty headers transfer ByteStream, guard, permit, original end
and length accounting together into Download custody. Empty objects follow the
same custody and `next_chunk` path before get returns, ensuring EOF/checksum
confirmation under the original end. A failed empty read returns the error;
there is no detached verification or special zero-length success shortcut.
A completed empty result stays successful after its old deadline.

The storage API gains no caller-deadline argument. A parent's shorter deadline
cancels get/Download by dropping its owner. After HTTP headers are sent the
existing HTTP handler deadline no longer governs response body writes; the new
storage original end does. An already-sent HTTP status cannot be replaced.

## S1 shared state, terminal transitions and cleanup

`Download` keeps immutable metadata outside the mutex for `metadata()`'s
existing borrowed return, an Arc<Mutex<State>>, and its owned optional timer
JoinHandle. State owns original end, remaining length, last consumer Waker and
exactly one state:

- Open(Resources): provider ByteStream, withheld final chunk if present,
  OperationGuard and OwnedSemaphorePermit. All adapter-owned active custody is
  here, not duplicated in Download, timer closure or a consumer future.
- Succeeded: optionally the already-validated final chunk awaiting its ordinary
  next read. No provider body, admission permit or live observation remains.
- Failed(ObjectStorageError): stable error, no payload or active resources.

One synchronous mutex transition removes Open(Resources) exactly once and
installs the terminal state. Clock/length/failure/EOF selection occurs under the
same lock; competing timer/body/drop paths cannot each finalize. Extracted
resources, operation recording and destructors run after unlocking; no guard is
held across await, wake, observation or resource destruction. Recover a poisoned
lock to perform cleanup using the existing repository approach; do not panic
while attempting final release.

| Trigger while Open | Selected terminal outcome and resource action |
| --- | --- |
| Clock at/after end, including after a provider poll returns | Failed(Unavailable), guard.fail(..., `timeout`); discard just-polled bytes and withheld chunk; drop body, guard and permit outside lock; wake pending consumer. |
| EOF before end and remaining = 0 | Succeeded; move held last chunk into terminal success, guard.succeed once; drop provider body, guard and permit. The final validated chunk may be returned immediately or on next poll. |
| EOF before end with missing bytes, extra bytes, or supported checksum failure | Failed(Integrity), current `content_length` or `checksum` class; discard held chunk and provider body and release resources. |
| Other body error before end | Failed(Unavailable), current `body` class, same resource release. |
| Download drop before end | Remove Open; drop resources/guard as cancellation; abort timer. If the original end has already elapsed while still Open, expiry wins and records timeout. |
| Unexpected timer termination while Open | Failed(Unavailable), existing `body` failure class; native panic hook retains diagnostic panic and operation observation is finalized. Never leave an active body with a dead timer. |
| Provider body poll unwinds | Remove active resources and record cancellation, abort timer, destroy outside lock then resume unwind; no leaked slot even if caller catches the panic. |

Terminal states are checked before the clock. Thus an earlier success/failure
never changes because its old deadline passes; repeated failed reads return the
same error and no payload. Success yields its held validated final chunk once,
then EOF. `is_end_stream` is true only for Succeeded with no final chunk; Failed
must not pretend to be clean EOF. `size_hint` reads length plus held final chunk
under the lock while Open and the remaining final chunk while Succeeded. Failed
returns `SizeHint::default()` (lower bound zero, unknown upper bound), never
exact zero. A terminal failure has no payload but still owes an error frame;
its hint must not advertise the transport's zero-length completion shortcut.
The next body poll returns the stable error. Metadata continues to report the
original object size; successfully completed bodies retain exact length.

This distinction is required by the resolved Hyper 1.11.1 HTTP/1 consumer:
`proto/h1/dispatch.rs:367-379` maps exact zero to Known(0),
`proto/h1/role.rs:935-946` selects a zero-length encoder, and
`proto/h1/conn.rs:596-604` makes the connection stop writing body frames.
`dispatch.rs:392-398` then discards the body without polling its error even when
`is_end_stream` is false. Unknown upper bound instead keeps a body-bearing
HTTP/1.1 response on the polling path (chunked framing if no Content-Length was
supplied), where the error interrupts the body. If framing was selected before
expiry, the existing positive exact length already prevents the false-empty
shortcut. A consumer's explicit original metadata Content-Length remains its
header authority; the adapter does not synthesize or rewrite response headers.
This changes only failure hints and does not promise to replace a sent status,
force body polling for HEAD/bodyless statuses, or reclaim transport-owned bytes.

Polling checks time under the lock before and after each ByteStream poll and
immediately before its chosen payload/terminal decision. No next_chunk future
owns the held last chunk across an await: cancellation of one read cannot lose
that chunk or restart the timer. It registers the current Waker before a
possible Pending. Every underlying Ready body result consumes Tokio cooperative
budget, including empty chunks and withheld-final-chunk progress; exhaustion
returns Pending, releases the lock and arranges the native reschedule. Provider
Pending restores the native budget and retains the provider's waker path.
The timer does not poll or buffer the provider stream.

A chunk handed out before end is caller-owned and can outlive expiry. A
partially accumulated `bytes()` buffer likewise belongs to its collecting
future: expiry releases active shared custody, not already yielded bytes. The
current collection allocation rule is preserved, pending separate PR #248.
There is no claim that bounded active slots bound all retained response memory.

## Timer ownership, termination and runtime boundary

Spawn exactly one timer only for a still-open returned Download, before it can
escape get. Timer owns a Weak<State> and original end, never a strong permanent
custody reference. Sleep to that end, upgrade only for synchronous expiry,
extract resources/terminal outcome under the lock, unlock, release and wake,
then end. An unpolled caller therefore cannot prevent cleanup. A timer that
loses to prior EOF/failure/drop observes terminal/absent state and does nothing.

The timer future captures a private exit guard **before its first poll**. Its
Drop upgrades the Weak only transiently and fails any unexpectedly still-open
state through the existing Unavailable/body path. Normal expiry, prior terminal
completion, and owner drop leave no Open state for that guard to change. This
closes cancellation-before-first-poll and panic exit without an unobserved
live-resource leak. The per-operation observation and native panic hook provide
failure visibility; no new metric identity or process task registry is added.

Download owns and polls/reaps the JoinHandle during body polls. On unexpected
JoinError it invokes the same fail-closed transition if still Open. EOF,
failure and Drop synchronously take/drop active resources and call abort on
an unfinished timer; they do not wait for timer scheduling to free the slot.
Drop cannot await: abort is a cancellation request, not a claimed join. The
remaining aborted timer holds only Weak/end bookkeeping until Tokio destroys
it. Tests use the owned handle/completion notification to establish actual
termination, including cancellation before first timer poll. No detached reaper
or never-ending supervisor is justified.

This is operation-owned timer work, matching existing gRPC body custody, not
process background work. Bootstrap gains no task, shutdown stage, budget or
storage root token. Normal parent cancellation drops Download; the same cleanup
works during drain and startup failure. Runtime shutdown destroys the timer
future and its exit guard. A retained successful handle may retain a completed
JoinHandle allocation, but no live provider body, task or permit.

## Documentation, projections and collision boundary

D1 is a guide/source-comment alignment, not a new cross-provider subsystem.
Update the current sentences that promise headers-only GET, indefinite reader
retention or one gRPC count; do not append a contradictory new paragraph below
them. The exact guide owners and profile markers are in the ownership map.
Keep object-storage lifecycle guidance accurate for operation-owned timers and
presigned slow readers. The production contract records
resource-scope obligations conditionally when the service retains/adopts the
capability. It currently has no profile markers. Link at file level only to the
always-retained configuration policy and integration, runtime-lifecycle and
persistence architecture owners; they already gate their detailed optional
guide links. Do not add a direct link to a removable guide, an anchor in a
removable section or a new marker in production-contract. Retain all D1
obligations: local-versus-fleet arithmetic, the 100 ms HTTP database reserve,
shared readiness and unresolved business class/backlog/CPU policy.

Auth-specific proving/source sections remain within their existing template
markers; opening admission stays outside them. Whole gRPC/object-storage crates
are already pruned by `scripts/lib/template_profiles.json`. In profile-bearing
guides, place changes inside their existing sections. Production-contract uses
the always-retained file-link rule above, so D1 requires no manifest entry or
new marker. Reuse existing projection cases; only a new auth-specific proving
section may require registration in the existing authn inventory.
`scripts/lib/template_init.py::_apply_markers` rejects any unknown marker and
requires exact source/inventory correspondence; no new profile or parser
behavior is selected.

Immutable PR #248 head `7420f7c37ab036c2642551a5e50d2ec7dfb1b737` changes Download
collection allocation and nonstreaming provider response bytes, and describes
unpolled retention. Those are collision evidence in
[dispositions](../recommendation-dispositions.md), not adopted implementation.
Keep bytes() allocation and provider response interceptors untouched here; this
work changes only required custody access inside bytes(). If #248 lands first,
refresh that narrow source/guide collision and preserve its accepted allocation
behavior; if assumptions disagree, reopen the smallest owner. Never import the
unmerged patch as an invisible prerequisite.

## Proof boundary and reopen

Implementation chooses deterministic cases beside the existing owners: G1 must
prove verifier-entry count across follower/saturation/cancellation and clones,
health/zero compatibility, independent terminal capacity and bounded rejection;
S1 must prove original-end expiry across headers/empty objects/all consumers,
unpolled body and last-chunk destruction, actual timer termination, post-expiry
no dispatch/payload/success, stable finality, cancellation-safe last-chunk state,
cooperative ready-frame progress, failure visibility when expiry precedes an
HTTP/1 response handoff, and exactly one observation/slot release. The framing
falsifier must exercise the consumer boundary: a Download that has autonomously
failed before response headers must not become a clean empty successful
HTTP/1.1 response through exact-zero optimization. A size_hint assertion alone
is not proof of that consumer behavior.
Existing checksum, length, response-body and profile tests remain parity proof.
Use drop-observable in-memory SDK bodies/current fixtures; no live provider,
RSS/load benchmark or new environment is required by this design.

No code/build/test/CI result is claimed here. Final validation follows the
Implementation owner, repository budget and shared lock, with fresh final
assembled review because concurrency safety changed. Reopen Technical Design
for a required new concurrency owner or a falsified cleanup/placement invariant;
reopen Definition for new behavior, configuration, dependency/feature admission,
provider contract or changed resource scopes. Evidence-only source/PR drift
reopens only its affected disposition. Planning is the next phase after the
independent Technical Design Review; this actor does not start it.
