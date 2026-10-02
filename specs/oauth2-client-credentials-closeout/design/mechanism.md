# OAuth closeout technical design

Status: ready; [fresh Technical Design review](review.md) passed.

Authority: [behavior spec](../spec.md), [Definition transition](../definition-transition.md),
[library decision](library-decision.md). Runtime baseline is
`67be869acea112af271ec8ba621cbc50ae9d36b7`; Definition-only commit `e9e57b3`
does not change the inspected implementation.
The accepted amended spec SHA-256 is
`bc78c10a723502a01bc40d91db174910e6fed6a9e18f1cc39e9bd1b862af3236`.

## Selected mechanism

Retain the current private adapter and its Tokio acquisition mutex, Moka
subject cache, admitted signer and bounded HTTP client. Add a small completed
failure record to service-token state, make token reuse require a representable
explicit lifetime, and use Moka's supported weigher for both retention targets.
Replace the detached refresh spawn with an explicitly owned per-integration
driver future. Neither foreground acquisition nor an individual background
refresh spawns an adapter-owned Tokio task.

These decisions preserve the current trust/wire/error authorities. No database,
readiness, authorization-server registration, resource retry, DPoP or generated
OpenAPI change is selected. The driver ownership changes composition as stated
below; it does not introduce a process-global OAuth registry.

## Service acquisition state and deadlines

The existing `Inner` remains canonical for immutable credentials, transport,
service token state and exchange cache. `Cached` retains a token and next
refresh eligibility and adds a sanitized optional completed failure
`{ error: AcquisitionError, until: tokio::time::Instant }`. The refresh queue
also has one pending flag protected by that same short synchronous mutex.
No synchronous guard crosses an await. The existing asynchronous acquisition
mutex serializes every actual service-token request, foreground or driver.

For a foreground service-token call:

1. Reject an elapsed caller deadline first. Reject a closed lifecycle owner
   before starting new work. A reusable cached token wins over remembered
   failure; its hit may nonblockingly request early refresh as below.
2. Otherwise await the acquisition mutex under the caller's absolute deadline.
   Recheck deadline and lifecycle after acquiring it, then reusable token, then
   a failure whose `now < until`. The failure returns its copied closed enum
   without a new token attempt or metric. Expired failures do not slide.
3. Start one acquisition. Its full fetch deadline is acquisition-start plus
   five seconds; its effective deadline is the earlier of that and the caller
   deadline. Signing, sending and bounded response admission spend that budget.
4. A successful admitted token clears the failure. Return the token once to
   this call; retain it only if it is reusable. Release acquisition ownership
   before resource dispatch. Waiting calls then independently reevaluate state.
5. A completed provider/transport/response/signing failure writes the closed
   error and completion-time plus one second before releasing the mutex. A
   full adapter-budget Timeout does likewise. A cancelled caller or a timeout
   caused by an earlier caller deadline does not write a failure. Dropping the
   request future releases both the fetch future and acquisition guard.

The implementation must preserve why a timeout occurred, rather than guessing
from the public `AcquisitionError::Timeout` enum alone. Compute whether the
caller shortened the attempt budget before entering the fetch; an elapsed
shorter caller deadline dominates a simultaneous result and is not published.
When the caller permits the complete five-second attempt, expiry of that cap
is a completed shared Timeout. Foreground lock waiting is not an acquisition
and creates no failure or acquisition metric. Every resumed waiter checks its
own deadline before reading a shared outcome or starting I/O.

Selective 401 eviction changes only the matching old token and its refresh
eligibility. It preserves the independent failure and pending-refresh state.
Expiry likewise cannot clear failure suppression. A successful acquisition
clears failure even if its valid token is request-only. There is no timer to
retry a failure: only a later eligible call or the existing refresh trigger can
start work. Background retry spacing remains thirty seconds.

## Token lifetime and exchanged calls

`into_token` distinguishes three cases. An omitted `expires_in` yields a valid
request-only token with no reuse or refresh time. A present lifetime uses
`started.checked_add(Duration::from_secs(value))`; failure, zero, or an expiry
reached at admission returns InvalidResponse. A positive, still-live,
representable expiry retains the existing ten-second reuse margin and refresh
window. `Token::is_reusable` requires `Some(until)` and `now < until`.

No request-only service token enters the reusable cache. A background success
with a request-only token has no requesting resource call, so it is discarded;
the old cached token remains usable only to its unchanged cutoff. That success
clears failure, and the existing thirty-second refresh spacing still applies.

Exchange remains Moka's per-key `or_try_insert_with` flow, with SHA-256 subject
keys, independent keys and no persisted acquisition failure. The installed
Moka 0.12.16 `future/entry_selector.rs` documents that only the initializer's
entry is fresh; waiters have `is_fresh() == false`. The initializer can use its
new admitted token once even when it is not reusable. A waiter can use a result
only while reusable; otherwise it conditionally removes that exact token and
re-enters the same coalescing operation under its original deadline until it
gets a reusable token or becomes the initializer itself. Once this call has
performed one successful acquisition, return its token immediately: there is
no refetch loop for that call's newly acquired request-only token. Each call
can execute at most one successful initializer; repeated waiting on somebody
else's result remains bounded by its own deadline.

This closes the existing second-result shortcut, which returns the second
Moka value without checking whether this caller fetched it or it is reusable.
It preserves concurrent reusable success/failure coalescing. Zero retention
in `ExchangedExpiry` is reclamation only; the explicit reuse/fresh decision is
the authorization boundary. Moka's wall clock must not substitute for Tokio
expiry admission, including in paused-clock proof.

## Two retention targets through one supported weigher

Let `B = 16 * 1024 * 1024` bytes and `C = exchange_cache_capacity`.
Configure Moka with `max_capacity(B)` and integer entry weight
`max(token.header.as_bytes().len(), ceil(B / C))`. The admitted count range
makes the minimum weight positive and representable; the existing response
ceiling makes every admitted header weight fit `u32`. Compute the division in
a wide integer type and convert only after the bounds are established.

Once maintenance settles, total weight at most B implies both actual retained
Bearer bytes at most B and entry count at most C. This is conservative: integer
rounding or mixed token sizes may retain fewer entries than a separate
two-dimensional eviction controller would. The default C=1024 has an exact
16384-byte minimum weight. For C=65536 it is 256 bytes. Accepted targets are
upper retention targets, so reduced retention is a cache miss cost rather than
a refusal of a valid token.

The requesting call retains its returned Arc regardless of Moka admission or
eviction. Weights count complete sensitive Bearer header bytes, including the
prefix, and do not count subject plaintext. `weighted_size` is an accounting
upper bound on actual payload, not an assertion that payload equals weight.
Concurrent eviction lag, live Arc references, keys/metadata and allocator cost
remain outside the target. Proof must settle `run_pending_tasks` and inspect
both entry count and actual retained-header sum. No new accounting map,
eviction listener, background sweeper, admission rejection or RSS claim exists.

The [supported Moka weigher](https://docs.rs/moka/0.12.16/moka/future/struct.CacheBuilder.html#method.weigher)
owns weighted eviction; the template supplies only this policy. A second cache
or byte counter would add synchronization and replacement accounting without
improving the accepted best-effort contract. Reopen only if independently
tight utilization of both dimensions becomes a requirement.

## Refresh lifecycle and material flow

The selected public construction seam is
`Credentials::prepare(Options) -> Result<(Credentials, RefreshDriver), ConfigurationError>`.
The old unmanaged `Credentials::new` route is removed; existing private
fixture construction becomes an internal builder with the same ownership
result. `RefreshDriver` is non-cloneable, must-use, redacted, and exposes
`async fn run(self, shutdown: impl Future<Output = ()>)`. HTTP/gRPC binding,
Options, request execution and closed acquisition errors retain their shapes.

The concrete reference graph is:

- Every external Credentials and authenticated-client clone shares
  `Arc<Owner>`. Owner owns `Arc<Inner>`, one bounded refresh sender and the
  sending half of a final-owner-loss oneshot. Driver never owns Owner.
- Driver owns a separate `Arc<Inner>`, the refresh receiver and the oneshot
  receiver. Final Owner destruction closes the oneshot without requiring an
  async destructor. Keeping Inner alive does not count as external ownership.
- One queue slot carries `{ current: Arc<Token>, deadline: Instant }`.
  `refresh_pending` prevents an active attempt from accumulating another
  scheduled refresh. Scheduling sets deadline to schedule time plus five
  seconds and advances existing refresh eligibility by thirty seconds, then
  uses nonblocking `try_send`. Failed admission clears the pending flag.
- Driver waits on shutdown, owner disappearance or that bounded queue. It
  executes each attempt inline and observes its outcome itself. There is no
  child JoinHandle, detached supervisor, blocking destructor or generic task
  tracking framework.

A cache hit schedules only when refresh is due, no refresh is pending and no
completed failure is currently suppressing acquisitions. The driver races
shutdown/owner loss against the entire refresh future. Its original deadline
wraps acquisition-lock waiting and the fetch together. After locking, recheck
time, cancellation, current-token pointer identity and suppression. A replaced
or evicted token, expired budget, cancellation or active failure ends this
scheduled attempt without a provider call. A wait that ends before acquisition
starts has neither a fetch metric nor a new shared failure.

Once a provider attempt starts, its cap remains the scheduled deadline; no new
five seconds starts after lock waiting. A deadline that expires during that
actual attempt is the completed background-budget Timeout and is shared for
one second. Owner-loss/shutdown cancellation is not a completed provider
failure. Completed errors publish through the same service failure path and
the driver logs only existing sanitized fields. Success retains a reusable
replacement, clears failure and preserves thirty-second retry spacing. Every
terminal path clears the pending flag. This flag is independent of token
replacement/eviction, so those mutations cannot admit overlapping scheduled
work while the driver is still finishing the previous attempt.

On shutdown or final owner loss, driver first closes admission, drops its
inline lock/fetch future and guards, clears pending state and returns. Its
caller observes that return. If cancellation and completion race, one
already-started operation may finish, but no subsequent attempt is admitted.
Dropping an unpolled or running driver closes its receiver and destroys its
owned future; surviving Credentials reject new work through the accepted
closed-owner rule. Already-started foreground effects remain caller-owned.

The closed flag is the refresh sender's receiver-closed state; it is terminal
and requires no extra atomic or reset path. Driver `run` closes the receiver
before dropping active work on its normal cancellation path; dropping Driver
also drops that receiver. HTTP execute and gRPC call inspect this state at
their entry boundary, before any cache fast path, form construction or resource
call: closed plus elapsed deadline is Timeout, closed with remaining budget
is Unavailable. The gRPC synchronous reusable-token branch must pass this same
gate; checking only the shared async authorize function would leave a bypass.
For active owners the existing conflict/subject validation remains canonical.
After admission, calls may complete under their own budgets; rechecking closure
before a new foreground attempt can end them early but cannot cancel an
already completed external effect. Service and subject paths both pass the
gate, and no hit can reopen a closed owner.

### Concrete composition and completion owner

The template has no concrete production integration constructing this adapter.
The guide therefore owns the runnable composition recipe; adding an unused
bootstrap registry would invent a production consumer. A derived integration
must retain and poll the returned driver from its asynchronous lifetime owner.
If that composition chooses to spawn it, its handle must remain owned and
awaited; a spawn-and-forget recipe is invalid. Do not blindly register it in
the template's always-running background JoinSet: that root currently treats
any early task return as a process failure, whereas final integration-owner
release is expected completion here. Concrete composition must distinguish
that expected completion from task panic in its existing lifecycle control.

The production recipe passes its existing shutdown signal as the `shutdown`
future and awaits driver completion in the existing background-join phase.
That phase precedes dependency drop, so shutdown cannot rely solely on final
client drop. Final client drop independently terminates the driver for shorter
integration lifetimes. A scoped example can jointly await integration work
and `driver.run(std::future::pending())`, where the work future owns and drops
all Credentials/client clones before returning. Neither recipe adds a new
public shutdown method or budget knob. Runtime teardown drops the owned driver
future and its inline work; normal completion evidence comes from its actual
composition await, not a test-only observer.

## Ownership and planning inputs

Placement is forced by existing adapter and documentation authority. Keep the
runtime change in the existing `crates/infra-oauth2-client-credentials/src/lib.rs`;
no crate, module, generic token source, feature port or service registry is
needed. Owner/RefreshDriver separate externally counted handles from work
completion; deleting that distinction reintroduces self-retention. The driver
uses installed Tokio channels/selection and remains this adapter's policy,
not a reusable task supervisor.

| Responsibility | Exact owner/action | Boundary and cleanup | Proof/reopen |
| --- | --- | --- | --- |
| Failure suppression, expiry admission, retention, external-owner counting and driver | Existing OAuth `src/lib.rs`; public prepare/driver seam, private state | No new crate edges; driver holds Inner, never Owner; request futures own foreground guards | Existing `src/tests.rs`; reopen Definition for observable policy change, Design for a different lifecycle mechanism |
| HTTP/gRPC delegation | Existing `src/lib.rs` and `src/grpc.rs` | Continue one acquisition boundary and original caller deadline, no replay | Existing `src/tests.rs` and `src/tests/grpc.rs`; only composition/type access adapts |
| Fixture and real-provider lifetime ownership | Existing `src/tests.rs`, `src/tests/grpc.rs`, and `src/tests/keycloak.rs` under the adapter | Every fixture retains drivers and observes completion; explicit cancellation handles clones surviving fixture cleanup | Reuse current local and CI-owned proof; no new runner |
| Explicit Tokio features | Existing adapter `Cargo.toml` | Add normal sync/macros only if used; no package/version/backend change | Locked normal no-gRPC graph/build and selected existing feature gates |
| Operator/API/lifecycle guidance | `docs/outbound-machine-authentication.md`, `docs/outbound-machine-authentication-decisions.md`, `docs/service-to-service-authentication.md`, OAuth paragraph of `docs/architecture/runtime-lifecycle.md`, affected crate API docs | Explain source migration/real driver await, failure window, request-only omission, invalid overflow, best-effort retention and dated library disposition | Static consistency/docs-check; preserve template markers |

The required Rust source owners are existing lib.rs and grpc.rs, including the
latter's synchronous reusable-token gate. Tests stay beside these owners.
Implementation selects fixture details and regression cases within the proof
boundary; it does not choose a different lifetime or caching mechanism.

## Proof boundary and stop

Nearest falsifiers are the spec's provider-refusal burst/recovery, independent
deadline and cancellation, suppression surviving cutoff/401, missing/overflow
lifetime in both grants, settled byte/count retention, and final-owner versus
surviving-clone refresh completion. Include the composition scenario across
refresh refusal, cutoff and later recovery. Distinguish a queued attempt that
never acquired the mutex from an actual timed-out acquisition in metrics.
Use observed driver completion, controlled provider gates and Tokio time;
sleeping or aborting without awaiting does not prove teardown.

The assembly review includes HTTP and gRPC mapping/deadline/no-replay behavior,
the documented public construction migration, profile removal/retention and
truthful library rationale. Existing local tests and required CI-owned OAuth
integration/profile gates remain their current authorities. No live-provider,
performance, RSS or deployment claim follows from this design.

Planning owns task boundaries and final validation routing. Technical Design
stops after its fixed candidate passes fresh read-only review and produces its
transition; no runtime code or tests are authored in this phase.
