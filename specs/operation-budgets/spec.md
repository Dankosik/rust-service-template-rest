# Specification: operation budgets

Status: ready. Definition owns behavior; Technical Design owns mechanism and
placement. Requester meaning: [Intent](intent.md). Factual baseline and limits:
[supporting research](research/baseline.md).

## Outcome and boundary

One admitted operation has one fixed monotonic cutoff and cancellation scope.
Business code can observe and propagate them without depending on a concrete
provider. Existing dependency ceilings may shorten the operation; they never
extend or restart its parent. This change closes propagation and resource
lifetime gaps; it does not claim that current durations fit every production
service.

The changed surfaces are inbound context delivery (HTTP, gRPC, messaging),
request-bound authentication waits, cache and object-storage calls, outbound
HTTP cutoff enforcement, and OAuth plus gRPC composition. Existing jobs already
expose an attempt deadline and cancellation; retain that authority and make it
usable with the common budget surface without changing attempt policy.

## B1. Admission, propagation, and cancellation

HTTP's existing admission instant and request cutoff remain authoritative.
gRPC opening uses the existing admission instant and the smaller of the local
opening cap and valid caller `grpc-timeout`. Messaging creates its context when
the existing handler attempt begins, with its existing handler cutoff and
delivery cancellation. Durable job attempts retain their existing origin.
The context is available to the business handler before it starts dependencies.

A child operation admitted at time `t` with local ceiling `L` has cutoff
`min(parent cutoff, t + L)`; subsequent stages use that fixed cutoff. Expired or
cancelled contexts refuse new provider dispatch. Queue/permit wait, request
preparation, credential wait, connection establishment, dispatch, and bounded
body completion all spend their applicable operation budget. Synchronous
preparation cannot move the absolute cutoff; a post-preparation expiry check
prevents late dispatch. This is cooperative asynchronous cancellation, not
preemption of arbitrary CPU work or proof that a remote effect rolled back.

Child cancellation cannot cancel a parent, sibling, or process-owned shared
refresh. Parent cancellation reaches request-owned child work. Cancellation
must not leave a detached task or owned permit alive without a named finite
cleanup/lifetime owner. A cancelled/expired caller cannot receive a newly
reported successful result from its unfinished operation. This concerns the
terminal caller outcome; it does not erase a provider's already definitive
mutation result at an internal adapter boundary (B5).

For standalone adapter use without an inbound parent, the adapter's existing
local ceiling supplies a finite operation budget. No new universal duration is
introduced. A retained convenience API must route through the same enforcement
owner; it must not silently discard an explicitly supplied context.

Nearest falsifier: spend time in one stage, enter a second stage, and observe
that its absolute cutoff and cancellation lineage remain bounded by the first
operation; an already expired parent produces no dependency dispatch.

## B2. Request opening and response streams

An HTTP handler/opening timeout does not become a new global body-transfer
policy. For inbound gRPC, a valid supplied caller duration continues to bound
the response stream from the original admission instant. When the caller has
not supplied a valid duration, the current contract still has a bounded opening
and no new generic stream-lifetime cap. Exposing the opening context must not
accidentally cancel such streams after the opening cap.

Work deliberately continuing in a response stream has a distinct, explicit
lifetime owner; it must not reset and present the expired opening budget as the
same live operation. New dependency calls from that owner use its applicable
remaining lifetime, if bounded, plus the dependency's existing finite ceiling.
This distinction must be visible in the public API/documentation so feature
authors cannot mistake opening completion for body completion. Object-storage
resources additionally obey B6 regardless of transport stream policy.

Nearest falsifier: a gRPC stream with an explicit short caller deadline ends at
that deadline; an otherwise valid stream without that header is not newly
terminated solely because the opening timer passed after headers.

## B3. Inbound authentication and cache

Authentication waits honor the inbound opening context, including introspection
coalescing and unknown-key JWT refresh waits. A caller that expires must stop
waiting without dispatching protected business work. This does not cancel a
process-owned JWKS refresh or another live waiter's work. Shared work retains
its existing independent finite provider ceiling and lifecycle owner; a new
request-owned provider call uses the smaller of parent remainder and that
ceiling. Authentication remains fail-closed with existing sanitized transport
failure ownership; expiry cannot become invalid-credential evidence or success.

Cache commands accept the caller context and bound acquisition plus dispatch by
the smaller of parent remainder and their existing command limit. There is no
new command replay, no change to feature-owned fallback or cache consistency,
and no coupling of a request's cancellation to connection-supervisor recovery.

Nearest falsifier: one short authentication waiter expires while another can
still consume the shared result; a nearly expired request cannot obtain a fresh
full cache command duration. Existing cache unavailability remains degradable
only according to its consuming feature's policy.

## B4. Outbound HTTP and OAuth/gRPC

Outbound HTTP enforces its accepted absolute deadline directly across bounded
response-body completion. Computing a remaining duration before synchronous
preparation must not yield a later terminal cutoff.

An authenticated outbound gRPC call establishes its cutoffs on entry to the
composed client, before credentials. Under the default `FullRpc(L)` policy,
one total cutoff is the minimum of an explicitly propagated parent cutoff,
a valid supplied caller duration measured from that entry, and `entry + L`.
Missing/invalid caller duration uses the local ceiling. Cache hits, token
contention, token exchange, credential refresh waits, client readiness,
queueing, response DATA, and trailers preserve that same total cutoff.

The existing explicit `OpeningOnly(L)` policy remains supported with OAuth.
Credential acquisition, readiness, queueing, and response headers share one
opening cutoff: the minimum of `entry + L`, parent cutoff if supplied, and
caller duration from that same entry if valid. After headers, only the supplied
parent/caller cutoffs bound lifetime, using the earlier one when both exist;
the local opening cutoff is not a lifetime cap. Without either supplied bound,
the explicitly owned stream retains its existing uncapped lifetime. Parent
cancellation still terminates its child call. This is the B2 stream-lifetime
distinction, not a second allowance for opening work.

Under either policy, credentials receive no separate prelude followed by a
fresh opening/RPC allowance, and may fail earlier at the existing fetch
ceiling. Forwarded caller timeout metadata is calculated from the remaining
caller lifetime, including credential wait; an explicitly propagated parent
bound can only shorten it. Local enforcement must retain the applicable
opening/lifetime cutoff despite metadata rounding and downstream queueing.

The existing authenticated HTTP composition already uses one fixed deadline;
preserve it. Token rejection can invalidate a token for a later operation but
must not replay the current protected effect. Authorization/OnBehalfOf trust,
secret handling, and existing sanitized failure provenance remain unchanged.

Nearest falsifier: without a timeout header, a slow credential acquisition
leaves only the remainder of the local full-RPC budget, including a slow body;
a cached-token call honors the same total bound. Expiry before resource
dispatch sends no resource request. With `OpeningOnly`, credentials consume
the opening allowance but a stream without parent/caller cutoff can continue
after that opening cutoff once headers arrived; a supplied cutoff still ends
the stream from the composed call's original entry.

## B5. Truthful effects and retained retry owners

Timeout or cancellation means the caller stopped waiting. It never proves that
an in-flight SQL commit, object mutation, message publish, or remote request
did not happen. Preserve `CommitUnknown`/`OutcomeUnknown`, immutable logical
identities, and existing reconciliation/replay contracts. Provider boundaries
must retain any stronger known outcome; generic budget handling cannot turn
unknown finality into definitive rejection or success.

In particular, a cooperative SDK mutation poll begun while the context is live
may synchronously establish definitive success and return at or after the
cutoff. The adapter preserves that confirmed success as its existing success
result; it must not reclassify it as `Unavailable`, `OutcomeUnknown`, or a new
retry-eligible error merely because a post-poll clock check observes expiry.
The result means the mutation is confirmed, not that the original caller met
its time budget. The original HTTP/gRPC/messaging/job terminal owner still
applies its existing expiry and settlement policy; an internal known result
does not authorize a late successful terminal response.

This does not permit another provider poll or new dispatch after stop is
observed at an owned boundary, or reset the deadline. An in-progress
synchronous poll is not preemptible. If the dispatched operation is still
pending when timeout/cancellation wins, finality remains unknown; a definitive
provider rejection retains its existing classification. No additional retry
eligibility follows from any of these cases.

Existing HTTP idempotency/webhook transaction cutoffs and their 100 ms response
reserve remain unchanged. Generic PostgreSQL `in_tx` is not turned into a retry
owner or a new transaction-duration policy in this task. Native SQLx return
custody remains unchanged. No new retry loop or breaker is accepted. Existing
jobs, webhook, outbox, messaging, SDK, and connection-recovery owners retain
their attempts, backoffs, identity, settlement, and cancellation semantics.
Messaging handler timeout still feeds its existing failure/settlement path; it
must not ACK success or erase an ambiguous handler effect.

The documentation must distinguish an accounted attempt limit from a lifetime
physical network-call bound: job refunds/drain/snooze, broker redelivery or ACK
loss, and SDK credentials/recovery can invalidate such a bound. Business retry
eligibility remains with the effect owner; inheriting a budget grants no retry.

Nearest falsifier: cancellation after dispatch retains an ambiguous mutation
outcome and triggers no automatic second effect; a timed-out messaging handler
keeps the existing settlement and redelivery behavior.

## B6. Object-storage body and resource lifetime

Every request-bound S3 call accepts the parent context. Its admission,
preparation, SDK operation, and body consumption share the smaller of parent
cutoff and the existing local operation limit, fixed when the call begins.
The SDK remains the read-retry owner; mutation operations retain one attempt
and their current unknown-outcome semantics.

Buffered download collection is bounded in bytes by the existing object limit
and in elapsed time through confirmed EOF by that fixed operation cutoff.
Bytes that fail length/checksum validation or time out before confirmed EOF
cannot be returned as a successful complete object. Partial stream bytes
already consumed are not retroactively retractable; final status remains
failure and the final confirming chunk keeps its current integrity semantics.

A live streaming download has an explicit finite storage-resource lifetime
using the same fixed operation cutoff, rather than an idle/per-frame timeout
that restarts forever. Expiry, parent cancellation, drop, normal EOF, and body
error terminate ownership of the SDK body and release the admission slot.
This release must occur when the application retains the download but never
polls it; a timer checked only during polling cannot satisfy the requirement.
No unbounded producer queue or detached producer is permitted. Transfer of the
download into an HTTP body transfers the explicit lifetime owner, not a promise
that handler timeout will enforce it. Presigned GET remains an available
alternative for transfers needing a longer independent lifetime.

Failure before headers uses the adapter's existing unavailable/finality
mapping. After headers, a body failure terminates the body through its existing
error channel; it does not rewrite an already sent HTTP success status or claim
complete-body success. Storage deadlines do not alter a presigned URL's own
expiry or feature-owned retention/authorization policy.

Nearest falsifier: a trickle body exceeds total time despite never stalling;
an unpolled retained download reaches expiry and another call can acquire the
slot. The same bounds apply to buffered collection and cancellation.

## Compatibility, proof, and decisions left to design

Keep current baseline values: HTTP/gRPC local 8 s, inbound auth 3 s,
PostgreSQL acquire 3 s/session statement 8 s, cache 100 ms, S3 operation 5 s,
and PostgreSQL response reserve 100 ms. Their owners remain canonical.
No global duration tuning, startup/readiness redesign, gateway retry rule,
retention policy, or new infrastructure is included.

The intentional behavior tightening is earlier termination where a stage
previously restarted its budget, plus finite S3 body/resource lifetime. Public
Rust API adaptations must be documented and all template-owned consumers and
optional profile carriers updated together. Feature/provider dependency
direction and generated-source ownership remain unchanged.

Implementation chooses focused proving cases and commands under the repository
validation budget. Proof must distinguish pre-dispatch refusal, shared-wait
isolation, full-body versus header completion, no-poll resource release, and
ambiguous effects without replay. Reuse adequate existing coverage; no new
benchmark campaign or live-provider certification is required to establish
this local behavior. Actual database behavior claims retain their mandatory
real-database proof owner.

Technical Design must choose the minimal public carrier and crate placement,
transport extraction/phase ownership, parent-aware adapter APIs, truthful error
mapping, owned S3 lifetime mechanism, and supported native deadline APIs. It
must compare existing dependencies/standard mechanisms before introducing any
crate or general mechanism, and reconcile current source against open reference
PRs without treating them as the baseline. No requester-owned input remains.
