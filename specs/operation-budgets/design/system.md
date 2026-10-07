# Operation budgets: technical design

Status: ready. Candidate: D1-r1. Baseline:
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`.

Authority: [accepted behavior](../spec.md), [Definition result](../definition-result.md),
[intent](../intent.md), and [baseline evidence](../research/baseline.md).
This design closes mechanism and placement only. Implementation chooses the
focused tests and commands; no implementation or runtime proof is claimed here.

## Decisions and native mechanism comparison

| Decision | Selected mechanism and decisive constraint | Rejected alternative, accepted cost, reopen condition |
| --- | --- | --- |
| Business-readable budget shared by inbound and provider crates | A small always-retained `operation-context` leaf with a monotonic deadline and Tokio cancellation lineage. Current users cross HTTP, gRPC, auth, cache, S3, jobs and messaging; none may depend on another transport to name this contract. | Putting the type in `infra-http` creates provider-to-transport edges; putting it in `service-failure` merges lifecycle with closed failure identities. Repeating tuples loses one child-budget/cancellation contract. Accept one workspace crate with two production values and no provider or transport dependencies. Reopen if a current neutral owner with the same responsibility emerges. |
| Deadline representation | Reuse the current `infra-grpc::call::Deadline` origin-plus-duration arithmetic in the leaf, including overflow-safe waiting for legal large gRPC durations. Add conversion from an existing absolute Tokio instant, remaining-at-one-instant, and finite-child cutoff access. | Raw `Instant + caller_duration` can overflow; a general scheduling/retry framework owns no accepted responsibility. Normal finite child operations use an exact absolute `Instant` for `timeout_at`; very large caller lifetimes retain original origin/duration. |
| Deadline enforcement | Existing `tokio::time::timeout_at` or a biased deadline/cancel select, plus explicit checks immediately before dispatch and terminal/complete-read success. Confirmed mutation results follow the finality rule below. | Native timeout futures alone may poll an immediately ready operation after expiry. No claim of CPU preemption or a nanosecond physical-return bound. Synchronous observation after the terminal decision may delay return without changing that decision. |
| Provider transport bounds | Keep reqwest 0.13.5 per-request timeout and retry-never; tonic 0.14.6 `Request::set_timeout`; AWS SDK operation/attempt overrides and existing read/mutation retry split. Outer fixed cutoff remains authority. | SDK timeouts alone do not deliver one shared parent budget or unknown-effect semantics. No new external dependency or version/feature upgrade is needed for the shared carrier or adapters. |
| Introspection caller isolation | Keep Moka 0.12.16 positive retention/coalescing and its existing initializer takeover. Caller waiting is bounded outside Moka; cached/shared work does not carry one caller's cancellation token. | A `Shared`/weak registry could preserve exactly one physical exchange across initializer cancellation, but B3 requires live-waiter isolation, not that additional guarantee. Reject its second per-key registry and generation cleanup. Native Moka may restart initialization when its initiating future is dropped; other callers keep their own original cutoff. No application replay loop is introduced. Reopen if exactly-one exchange survival becomes an accepted requirement. |
| S3 body lifetime | Adapter-local resource cell and weak-reference expiry task, adapting current gRPC `call.rs` ownership pattern. It removes SDK body, final-chunk buffer, observation guard and permit at the fixed cutoff/cancellation even without body polling. | tower-http 0.7.1 `DeadlineBody` is poll-driven, so it cannot release an unpolled retained body. A producer task/queue adds buffering and background I/O; reject it. Accept one small timer task per admitted live download, bounded by existing storage concurrency. Reopen if the SDK offers autonomous owned-resource expiry with equivalent EOF/error behavior. |

The installed mechanisms are reused at their resolved versions; there is no
new registry package, dependency upgrade, generic retry trait, breaker, budget
configuration key, or universal response reserve. The existing PostgreSQL
terminal owner's 100 ms reserve is preserved verbatim.

## Shared carrier and public contract

`crates/operation-context/src/lib.rs` owns public `Deadline` and
`OperationContext`. The latter contains an optional immutable deadline and one
`CancellationToken`; `None` is permitted only for an explicitly named response
lifetime or a standalone parent before an adapter applies its finite ceiling.
Every admitted HTTP/gRPC opening, messaging attempt, job view and provider
operation has a finite cutoff. It is not serialized or accepted from untrusted
HTTP metadata. No ambient task-local context is introduced.

The public surface supplies creation from an existing absolute cutoff or
origin/duration, deadline/remaining observation, child derivation with a local
ceiling, cancellation observation and a closed stopped reason
(deadline/cancelled). It may expose cancellation of the current logical scope;
child construction always uses `child_token`, so a child's cancellation cannot
cancel a parent or sibling. Clones denote the same logical scope. The carrier
owns no spawned task, provider error mapping, retry, telemetry, cleanup, or
response-reserve policy. It provides stop/check primitives, not a generic
provider-operation runner. Duration overflow never turns a valid finite caller
budget into absence or panics.

A child admitted at one sampled instant `t` chooses
`Dchild = min(Dparent, t + local_ceiling)` and keeps it immutable. A standalone
adapter chooses its current local ceiling. Every later stage uses the same
child; it must not call the child constructor again to obtain a fresh allowance.
Expired/cancelled state is checked before expensive preparation where possible,
after synchronous preparation, immediately before effectful dispatch and before
an unfinished operation reports terminal success. A live-started synchronous
provider poll may establish a definitive mutation result before returning
control; that result retains its current adapter classification even when the
clock has crossed D. The terminal owner separately checks its original cutoff.
No further provider poll/dispatch starts after stop is observed, and no budget
restarts. Already known stronger outcomes survive error mapping; cancellation
never establishes rollback.

Adapters retain existing standalone convenience methods and add explicitly
named `*_with_context` methods for request-bound use. Both delegate to one
private enforcement path, with standalone calls synthesizing only the existing
local ceiling. This is the current standalone-use capability, not a second
implementation or retry path; remove a convenience surface if that capability
is removed. An explicitly supplied context, including a standard HTTP/gRPC
request extension, is never overwritten by a later local default.

## Admission, opening and response ownership

### HTTP

The hardened chain's current request-budget position fixes the opening origin
before auth/body handling. One middleware owner installs `OperationContext` in
request extensions and enforces that exact cutoff across `next.run` with the
existing 504 Problem identity/detail. This replaces the independent relative
Tower timeout at that same position; request IDs, nosniff, telemetry, admission,
panic recovery, body limit and fallback order stay unchanged. The existing
`RequestDeadline` remains a read-only projection of this exact instant for
idempotency/webhooks, with no second clock or reserve.

`infra-http::RequestContext` is a thin `FromRequestParts` wrapper exposing the
neutral `OperationContext`, with a sanitized internal rejection if the hardened
chain was omitted. It introduces no new wire status. A separate
`infra-http::ResponseContext` wrapper exposes a context for deliberately
continuing work in a response; its deadline is absent unless a route deliberately
supplies a finite response owner. Both use the call cancellation lineage, but
the response context never presents the expired opening cutoff as a live one.
The opening wrapper and explicit response wrapper are distinguishable types.
After `next.run` resolves, the transport checks its original cutoff before
committing any response, including an authentication/cache 503 that became
ready at the cutoff. Expiry wins as the existing 504; provider-ceiling failure
while the opening remains live keeps its existing 503 provenance. The gRPC
opening owner applies the same terminal ordering with its existing deadline
status. No new failure code or reserve is introduced.

The opening future owns a cancellation-on-drop guard. Successful header handoff
moves the call cancellation guard to a thin response-body wrapper that cancels
on body drop, error or EOF. Header success alone does not cancel storage work
transferred into that response body. This wrapper adds no generic HTTP body
timeout; a returned S3 download retains its independently fixed finite storage
cutoff. No autonomous request timer is added after HTTP headers.

### Inbound gRPC

At the existing `router::deadline` origin, install the finite opening
`OperationContext` before authentication. Also install a distinct public
`infra-grpc::ResponseContext` containing the caller lifetime from that same
origin, or an explicitly unbounded response lifetime when no valid caller
metadata exists. Both are accessible through tonic request extensions; neither
is derived from the other after headers.

Opening cutoff is `min(origin + local_cap, caller_cutoff)`. Stream cutoff is
caller cutoff only. A caller duration longer than the local opening cap can
therefore outlive that opening cap. Missing/malformed `grpc-timeout` keeps the
existing bounded opening and uncapped generic stream lifetime. Large legal
values retain overflow-safe arithmetic.

The current response terminal owner in `call.rs` takes the call cancellation
lineage together with body, upload and capacity. It handles supplied lifetime
expiry/parent cancellation autonomously, including unpolled response bodies;
EOF/error/drop stops the timer and drops those resources. Opening expiry or
abandonment cancels the call. Successful headers transfer cancellation custody,
without cancelling the response merely because the opening budget later ends.

### Messaging and jobs

`consumer::run_handler` samples its existing start once, derives a context from
`HANDLER_TIMEOUT` and its existing child cancellation token, and passes it
through `Registry` to the typed handler instead of the cancellation-only
argument. The existing timeout/panic/failure/settlement path remains authority;
it uses the same fixed cutoff, and the context guard is cancelled when the
invocation ends. A late ready success is not ACKed after the cutoff. Broker
ACK/NAK, redelivery, DLQ and shutdown budgets remain separate existing owners.

`Job::context()` returns a child view of the existing attempt deadline and
cancellation token; it introduces no new attempt origin, timer or cancellation
of the engine. Existing `deadline()`/`cancellation()` APIs remain projections of
the existing attempt owner. Queue claims, refunds, snooze and replay identities
are unchanged. Current webhook/outbox consumers may propagate this view when
calling a changed context-aware adapter; their policy does not move.

## Authentication and cache flow

The HTTP and gRPC auth boundaries call a context-aware `Verifier` entry. The
opening context governs envelope preparation, caller wait and the final
pre-business-dispatch check. Parent expiry maps to the transport's existing
opening timeout; explicit caller cancellation ends its work through the existing
sanitized unavailable/cancelled ownership, never invalid credentials. Provider
failure before parent expiry retains existing authentication-unavailable
mapping. No verified principal is delivered from an unfinished expired wait.

For JWT, only the unknown-key caller's refresh subscription is bounded by its
context. The bootstrap-owned refresh worker and its finite provider ceiling
remain independent. A request cannot cancel the worker or another subscription.

With introspection caching disabled, the new request-owned exchange uses the
smaller of parent remainder and the existing 3 s provider limit, fixed before
form construction. With caching enabled, positive lookup and Moka coalesced
initialization remain the existing owner. The caller waits under its own
context; shared initialization uses the provider's independent 3 s bound and
never captures the caller token. On initializer cancellation Moka's existing
native takeover can initialize again for a live waiter; that waiter retains its
original caller deadline. The contract does not count this as a promised
lifetime physical-call cap. Cache capacity, TTL/token-expiry policy, failure
non-retention, bulkhead, redaction and no-HTTP-retry configuration are unchanged.

Cache namespace `get`, `set`, `delete` context-aware entry fixes a cutoff before
key/command preparation, then passes it plus cancellation through acquisition
and its one command dispatch. Existing standalone methods use the same path
with the current 100 ms ceiling. Native connection-supervisor recovery retains
its process cancellation and backoff; it never receives the request token.
Failures keep `Unavailable`; a timed-out mutation can have landed, and there is
no command replay or new fallback policy. Readiness probing is unchanged.

## Outbound HTTP and OAuth/gRPC composition

Outbound HTTP retains the accepted absolute cutoff through synchronous
admission, DNS/connection, request dispatch and complete buffered body EOF.
Use `timeout_at(D)` rather than converting a duration before preparation into a
new relative timeout; check D/cancellation before dispatch and before reporting
buffered success. Existing `execute(request, deadline)` remains a convenience
for callers already holding an instant. Its implementation also honors an
explicit context carried on the request. `execute_with_context` enters the same
owner and local ceiling. Error/observation identities and no-replay semantics
remain current. Authenticated HTTP passes its same fixed context through
credential acquisition and resource HTTP; no refresh allowance restarts it.

`infra-grpc::client` owns a public opaque `PreparedCall`, constructed by
`Client::prepare_call(request)` at call entry. It holds that concrete cloned
client, the request, and already-selected cutoffs. It exposes the opening context
and mutable request metadata needed by the OAuth binding, then consumes itself
in `send`. It cannot be applied to another client's policy. Ordinary
`Client::call` uses the same prepare/send path. No I/O occurs in preparation.

The OAuth gRPC binding prepares this call before either cached-token or fetched
credential handling, then authorizes within its opening context and invokes
`send` on the same value. It does not reconstruct the resource deadline from a
rounded timeout header. Token rejection only invalidates future reuse; it never
replays this effect. Existing acquisition error provenance and subject trust
remain canonical.

For `FullRpc(L)`, opening and lifetime both use
`min(entry + L, propagated_parent_cutoff, valid_caller_cutoff)`.
For `OpeningOnly(L)`, that same minimum bounds credentials through headers;
after headers lifetime uses only
`min(propagated_parent_cutoff, valid_caller_cutoff)`, omitting absent bounds.
Without either supplied bound the response has an explicit uncapped lifetime.
Parent cancellation still ends it. The existing credential fetch ceiling can
end acquisition sooner; it is never added to the resource allowance.

Before resource dispatch, the retained caller/parent deadline is forwarded as
remaining `grpc-timeout` using tonic's native setter. Local `L` alone does not
create a new wire timeout. Rounding cannot extend local enforcement. Explicit
long caller duration under `OpeningOnly` remains a long lifetime while local
opening remains capped. Pre-dispatch expiry sends no resource request; response
DATA/trailers and upload custody retain the selected full-call/stream owner.

## S3 operation and owned body

Every public request-bound operation (put, streamed put, get, head, delete,
presigned GET) has a context-aware entry beside its existing standalone entry.
A single private admission/enforcement path fixes D before permit admission,
key/metadata/checksum preparation and SDK operation. Busy remains immediate.
Per-operation SDK config uses remaining operation time and the smaller of the
current read-attempt ceiling and remaining time; mutation operations still have
one SDK attempt. An outer absolute deadline/cancellation owner covers all work,
including credentials, and SDK config never becomes the final body lifetime
owner. Presigned URL expiry remains the existing separate signature parameter.

Before dispatch a stopped operation is unavailable with no new provider call.
After mutation dispatch timeout/cancellation remains `OutcomeUnknown` unless
current provider classification establishes a stronger rejection/integrity
outcome. Reads map unfinished timeout/cancellation to unavailable. A final SDK mutation poll is permitted only while the context is live. If that
poll synchronously returns definitive success, the adapter retains its existing
`Ok(())`, including when the non-preemptible poll crossed D. A definitive SDK
rejection likewise retains its current error. If the SDK future is still
pending when the biased deadline/cancellation branch wins, the result is
`OutcomeUnknown`. Do not poll it again after stop is observed.

The original HTTP/gRPC/messaging/job terminal owner enforces its own cutoff and
settlement policy; adapter confirmation does not authorize late terminal
success. In particular, the HTTP/gRPC postcheck already selected above still
returns its existing deadline response. This follows the reviewed B1/B5
clarification in Definition. Reject a new `CompletedAfterBudget` storage error:
it adds no needed effect knowledge and generic error-to-job mapping can make a
confirmed effect retry-eligible. Preserve the existing S3 result API and error
set; no effect is replayed by this layer. Standalone calls keep their existing
finite local ceiling and this same cooperative finality rule; physical CPU
preemption is not promised.

GET passes D and cancellation into `Download::open`, together with the exact
SDK body, observation guard and admission permit. `download.rs` owns one
`Arc<Mutex<State>>`; its live resource variant contains those owned resources,
length accounting and the withheld final chunk. Its expired/failed/finished
variants contain no SDK body or permit. A short mutex critical section may
poll the native body once; it never holds the lock across an await, calls an
arbitrary destructor or records terminal observation. State transitions take
resources out; destruction/observation/waking happen outside the lock.

A timer task holds only a weak state reference and waits for D or parent
cancellation. It does not poll the SDK body, produce bytes or buffer a queue.
Its one terminal transition drops body/final chunk/permit and records the
existing closed failure. The download retains the task handle; EOF, body error
and Download drop synchronously remove resources and stop/abort that task.
The timer's panic path is observed and turns the live download into the same
sanitized failure, using the existing gRPC terminal-owner pattern. It cannot
retain a strong cycle or resources after the download is dropped. Completion
proof observes timer completion/resource release, not merely a cancellation
request. Cancellation and the timer cannot produce duplicate terminal metrics.

`poll_chunk`, `next_chunk`, `bytes` and `http_body::Body` all use this state;
none obtains another allowance. The poll path rechecks stopped state before
releasing a chunk/final success so an expired ready body cannot win. Existing
length checks, checksum classification, withheld final-chunk rule and empty-body
EOF verification remain. `bytes` returns success only after confirmed EOF
within D. A retained unpolled body releases its slot at D. Partial streamed
bytes remain consumed; after HTTP headers the body error closes through its
existing error channel without rewriting status or claiming complete success.

## Proof boundary and release compatibility

Required behavior proof covers child cutoff/non-upward cancellation, expired
pre-dispatch refusal, terminal-success cutoff and confirmed-mutation retention, HTTP extraction and unchanged
terminal mapping, gRPC opening versus caller lifetime (including explicit long
caller and absent caller), OAuth cache-hit/miss time consumption under both
policies, auth waiter isolation, cache acquire-plus-dispatch, S3 trickle/EOF,
no-poll resource release and unknown mutation outcomes without replay. Existing
fixtures and adequate tests are reused; executor chooses exact cases and
commands. These are proof questions, not a new test-plan phase.

The final candidate has several crate/manifests changes: repository build/test
and routed validation apply. Profile/generated containment and selected real
cache/S3/messaging integration gates retain their existing CI owner. No live
provider, new infrastructure, benchmark campaign or live latency claim is
required. No PostgreSQL transaction mechanism changes or new database-behavior
claim is made; any later such change must reopen persistence and its real-DB
proof owner. CI/review evidence is distinct from local source/design evidence.

Earlier termination of restarted stages and finite S3 resource lifetime are
intentional. No baseline duration changes. Public Rust context entry points,
handler signature adaptation and explicit stream-lifetime guidance are updated
with template-owned consumers in one candidate; wire schemas remain generated
and unchanged unless implementation exposes an actual contract delta. Jobs,
webhooks, outbox, messaging and SDK attempts remain their owners: accounted
attempt limits are not lifetime physical network-call bounds because of
refunds/drain/snooze, redelivery/ACK loss and credential/recovery exchanges.

## Evidence and reopen conditions

Current source evidence is linked in the baseline. In this phase:

- `crates/infra-grpc/src/call.rs` already provides weak-state expiry, autonomous
  resource removal, timer observation and drop ownership; reuse its pattern
  locally, not its transport status/telemetry types.
- Moka 0.12.16 `src/future/value_initializer.rs` handles
  `EnclosingFutureAborted` by native initializer takeover. Its
  [documented coalescing API](https://docs.rs/moka/0.12.16/moka/future/struct.Cache.html#method.try_get_with)
  remains the selected owner. A bounded specialist compared a `Shared`/weak
  registry; the design rejects the stronger one-physical-exchange guarantee
  because it is not required by B3.
- Reference PR #242 exact head `b0f9899dd90ca1e1002e4dbf7575e3799a381e08`
  contains the relevant fixed-cutoff outbound patch, verified by a path-limited
  diff against baseline. Reuse that mechanism with context cancellation; do not
  import its unrelated auth-clock/cache changes.
- Reference PR #248 exact head `7420f7c37ab036c2642551a5e50d2ec7dfb1b737`
  was verified through GitHub metadata and the exact `download.rs`. It contains
  collection-allocation changes, not the no-poll S3 lifetime fix. It supplies
  no proof for that claim. #246/#247 remain unrelated transport/credential
  references; no patch or dependency change is adopted from them.

Reopen System Design if common context requires provider/transport dependencies,
SDK body cannot be safely removed independently of polling, or the prepared call
cannot preserve the selected interval through OAuth. Reopen Specification if
exactly-one shared auth exchange, a generic stream cap, new reserve/duration,
replay policy or externally visible failure identity becomes necessary.
Placement refinements may update only the affected ownership rows; they may not
move a protected authority or widen profile retention silently.
