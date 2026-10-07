# Time-contract mechanisms

Status: ready

Authority: [specification](../spec.md), [Definition result](../definition-result.md).
Source baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
This design closes the independent core only. Webhook retention remains with
the continuation owner and its pending user-owned decision.

## Fixed outbound end

`infra-outbound-http::Client::execute` samples Tokio `Instant` once on entry
and fixes `end = min(parent_deadline, entry + operation_timeout)`. Use checked
addition; an unrepresentable operation end cannot precede the representable
parent, so the parent remains the end in that branch. Reject `now >= end`
through existing `Error::Timeout` before admission. Admission, tracing setup,
header propagation, dispatch and buffered collection all consume this end;
none converts it back into a new relative timeout.

Pass that absolute instant into `exchange` and use Tokio `timeout_at` for the
whole exchange future. Check `now >= end` again inside the exchange immediately
before calling the transport. After `exchange(...).await` returns, full buffered
collection and the last await are complete. At this one operation-decision
boundary, before `Attempt::finish`, sample Tokio time and convert otherwise
successful completion to `Error::Timeout` if `now >= end`. Existing errors keep
their classification. Timer readiness alone is insufficient: Tokio polls an
immediately ready inner future successfully even when the deadline passed.

Fix that result, call `attempt.finish(&result)` once, and return the same result.
There is no deadline recheck after terminal observation and no second outcome
emission. `Attempt::finalized` retains its current once-only finish/drop role.
Synchronous tracing, histogram or other observation callbacks can delay physical
return beyond `end`; they cannot reopen the operation decision or dispatch.
No deferred reporting task, custom clock, transport wrapper or telemetry
framework is introduced. This is the reviewed
[operation-decision contract](../spec.md#outbound-deadline), not a promise to
interrupt arbitrary synchronous terminal callbacks.

Keep the existing observer's duration sample boundary. `Attempt::started` uses
`std::time::Instant` after span creation; `emit` evaluates its elapsed value at
the existing histogram record call, after span field recording and histogram
setup/lookup. Thus the sample includes intervening setup/terminal observation
work, can exceed the fixed operation budget, and excludes work after that
sample (including the record callback itself and any later reporting/cleanup).
It is neither the deadline-decision timestamp nor total physical-return time.
Do not move the sample, introduce another duration/outcome, or claim the metric
is capped by `end`. The fixed decision and its returned error/response are the
proof of the deadline contract; the existing span/metric report that same
outcome with their unchanged labels and duration semantics. Early rejection
before `Attempt::start` retains its existing observation behavior.

Cancellation drops the request/body future. An already dispatched remote
effect remains ambiguous; no retry or rollback is introduced. A scheduler can
delay wakeup, but cannot reset the chosen end. Existing bounded body collection
and target admission remain canonical. No transport wrapper or new task is
needed. Compared with a custom deadline future, `timeout_at` plus the two
boundary guards is the smaller change; revisit only if streaming or a new
transport exposes an effect beyond this buffered exchange boundary.

## Introspection evidence and conditional reuse

Keep public `Principal` and its equality/accessors unchanged. The claims owner
returns one crate-private `VerifiedIntrospection` value containing the
`Principal` and the already parsed `Option<u64>` not-before claim. Its lifetime
is one provider result, its coalesced waiters, and optionally one retained
entry. `exp` remains sourced from `Principal::expires_at`; do not duplicate it.
The only consumer of the extra `nbf` field is retained eligibility. Place this
small result beside introspection claim validation in `claims.rs`, guarded by
the existing introspection template markers. Validation order and exact stored
payload/token bytes do not change.

Adding private `nbf` to the common `Principal` would also work, but would make
JWT construction/equality and every principal carry metadata consumed only by
introspection retention. Re-parsing the retained payload on every hit would
repeat claim decoding. The private result wrapper isolates this current need;
reopen if another engine acquires the same real reuse responsibility.

Keep the existing positive Moka cache, now storing `VerifiedIntrospection`,
and add one private zero-retention Moka cache for fills. Both use the same
SHA-256 digest and immutable verifier trust context. The positive cache retains
its existing capacity and custom expiry. The fill cache uses `try_get_with`,
zero TTL, and the same configured capacity bound; it retains no completed
result for independent later lookups. Its value is a private tagged outcome:
`Fresh(VerifiedIntrospection)` or `Retained(VerifiedIntrospection)`. Its error
is the existing `VerificationError`. No public option, new dependency, manual
lock, generation counter, detached task, or global clock is introduced.

The distinction is necessary because Moka 0.12.16's conditional-entry API can
publish a retained value from its second lookup to joined waiters, just as it
publishes provider output. Neither `Entry::is_fresh` nor receipt through a
waiter proves a new provider verification. A single Result-valued conditional
cache cannot supply the required provenance. The zero-retention fill cache
contains only explicitly tagged outcomes produced by its initializer, so its
own second lookup cannot mistake ordinary positive retention for new evidence.

The flow is:

1. Require usable Unix time. Read the positive cache and sample current time
   for its eligibility: `now < exp` and `nbf` absent or
   `nbf <= now.saturating_add(30)`. Reuse the claims owner's leeway
   constant/predicate. A valid hit returns its principal; clock failure returns
   unavailable trust. Calendar-ineligible evidence takes the fill path without
   invalidating its key.
2. Call the zero-retention fill cache's `try_get_with` using the existing
   error type. Its initializer rechecks the current positive cache with a new
   clock sample. A now-eligible replacement produces `Retained(value)`. This
   check avoids unnecessary provider work and protects a newer eligible value.
3. Otherwise the initializer runs existing introspection admission, bulkhead,
   bounded HTTP, parsing and fresh claim validation. Clock usability is required
   before dispatch and sampled again for fresh validation. Provider success is
   inserted into positive retention and returned as `Fresh(value)`. Provider
   failure returns the existing error directly; no positive entry is inserted
   or deleted on that failure.
4. Moka shares the initializer's tagged outcome or error with its joined
   waiters. Every caller rechecks `Retained` against its own current calendar;
   if ineligible, it re-enters the miss path. Every `Fresh` caller samples its
   own usable current Unix time immediately before delivery and reapplies the
   existing fresh lifetime guards: first reject `now > exp.saturating_add(30)`
   as `Expired`, then reject `nbf > now.saturating_add(30)` as `NotYetValid`.
   Return the existing invalid-trust error on either failure; unavailable time
   remains unavailable trust. Reuse the claims owner's lifetime check, retaining
   equality and leeway. No provider retry or cache invalidation follows this
   caller-local delivery rejection. Fresh evidence inside expiry leeway still
   serves joined waiters, but a waiter resumed beyond that leeway cannot use
   the earlier validation time. No claims are reparsed and preceding provider
   parsing/claim errors keep their original order.

The decisive race is closed: if an initializer selected retained R, the clock
then advances and a caller joins that fill, its outcome remains `Retained(R)`.
That caller cannot turn R into fresh evidence: it rejects current calendar
ineligibility and follows the provider path. If the initializer actually
verified provider evidence, all its joined waiters receive `Fresh` instead.
The positive-cache fast path is also guarded by the caller's sampled calendar.
No pre-provider time sample is used for fresh validation.

Zero TTL removes completed fill results from subsequent lookups, including
Moka's second lookup. Already joined waiters still obtain the initializer
result directly from its waiter channel. Native `try_get_with` shares errors
without inserting them. Thus no remembered outage, negative cache, or
provider-per-waiter retry is introduced. An independent later request follows
normal lookup/provider admission. Dropping the initializing caller preserves
Moka's takeover by a surviving waiter; no detached fill task survives it. Both
caches live and drop with the existing verifier. In-flight work retains the
existing provider bulkhead and request cancellation owners.

`Retention` handles both `expire_after_create` and `expire_after_update`.
Calculate each successful value's fixed retention as the earlier of configured
TTL and its own expiry without leeway, with the existing payload-size bound.
Reads preserve the remaining duration. A new verified replacement never
inherits the previous value's remaining TTL. Clock-unusable retention samples,
oversized evidence and already-expired fresh successes receive zero retention.
Preserve checked far-future expiry handling when the Unix clock is usable.
No key invalidation is performed: a rejected old observation cannot remove a
newer eligible replacement. A retained ineligible entry may remain physically
present until its fixed expiry, but it cannot bypass any eligibility check and
is never a fallback for a failed provider attempt.

The existing OAuth identity-checked removal pattern is a viable building block,
but alone does not distinguish a joined retained result from fresh provider
output, and OAuth deliberately limits request-only success to its initializer.
The selected extra zero-retention cache buys exactly that missing distinction,
while leaving Moka in charge of coalescing, cancellation and expiry. Its cost
is one additional private cache and tagged fill outcome, with no second copy
of long-lived token evidence. Reopen if a future supported Moka API exposes
reliable provider-versus-retained provenance directly, or focused evidence
contradicts zero-TTL sharing. No broader caching abstraction is justified.

## Unavailable calendar time

Change crate-private `unix_now` to return `Result<u64, VerificationError>`;
pre-epoch `SystemTime::duration_since(UNIX_EPOCH)` failure becomes
`Failure::Unavailable` with one closed `VerificationReason::Clock` /
`clock` label. No token value or platform error text enters observations.
JWT propagates this at its existing fresh claim-validation boundary;
introspection propagates it along both cache paths and fresh validation above. Existing `Verifier` recording
and HTTP/gRPC unavailable-trust mappings remain the transport owners.
Signature, parsing, issuer/audience and normal-time claim order stay unchanged.
Recovery requires only a later usable clock sample; no latch or cached error.

Use a pure private SystemTime-to-Unix conversion helper for the unavailable
case. Where deterministic multi-step timing is necessary, allow an
engine-private verification helper receiving a narrow callback that supplies
the current Unix-time result, with production passing `unix_now`. Each call
samples at the same runtime boundaries as production. No callback is stored
in a public option, exported through `test-support`, or shared as a universal
Clock trait. Retention's existing explicit SystemTime calculation and Tokio's
controlled monotonic clock remain separate local mechanisms. Implementation
owns the smallest concrete seam and test arrangement within these boundaries.

## Retry-After rendering

`infra-http::Problem::into_response` retains the existing integer HeaderValue
construction. Compute `after.as_secs().checked_add(u64::from(after.subsec_nanos()
!= 0))`; render an ordinary successful result with the existing minimum of one.
The sole overflow case is exactly `u64::MAX + 1` seconds. Render that case with
the valid static decimal HeaderValue `18446744073709551616`. This is the exact
ceiling, not saturation or omission. A general u128-to-string/fallible-header
path adds no capability for this bounded domain; the installed HTTP integer
conversions cover u64 but not u128. No panic, allocation helper, new status,
problem code, body field, or OpenAPI schema change is required. Existing
problem-response ownership supplies the observable proof boundary.

## Redis TTL input contract

`CacheNamespace::set` changes to `Result<(), SetError>`, with a small public
typed error in the existing crate: `InvalidTtl` and
`Unavailable(#[from] Unavailable)`. Keep `get`, `delete`, probe and the shared
runtime `Unavailable` type unchanged. The distinction lets caller code repair
its supplied TTL instead of treating its programming error as an outage.
Use the declared thiserror dependency and sanitized static messages. Do not
include keys, bytes or credentials in errors.

Before constructing/dispatching a command or entering its observation guard,
compute `ttl.as_millis()` as u128, admit only `1..=i64::MAX`, and then convert
the admitted value to the existing `SetExpiry::PX(u64)` representation. This
preserves flooring, including a fractional duration just above the largest
admitted whole millisecond. Invalid input returns `SetError::InvalidTtl`
without a command, connection retirement, or fabricated server outage metric.
The admitted SET retains its one dispatch, existing command timeout and no
retry; transport/server failure maps to `SetError::Unavailable(Unavailable)`.
Redis alone decides whether server-now plus PX fits its absolute expiry.
There is no host-time estimate or tighter duration policy.

Update in-scope call sites and the crate/cache guide's TTL/error contract.
Keeping `Result<(), Unavailable>` would conflate programmer input with an
outage; a validated TTL newtype would broaden construction/call-site machinery
without another current consumer. Reopen only if a real shared validated TTL
boundary appears. Namespace validation is unchanged.

## Owners, evidence and handoff

Existing owners mechanically determine placement: outbound execution in
`infra-outbound-http`; auth clock, claims result and retention in
`infra-bearerauthn`; header rendering in `infra-http`; TTL admission/error in
`infra-cache`. Their existing documentation owns consumer guidance. No crate,
module, configuration, dependency, signature identity, schema, infrastructure,
runtime budget or rollout sequence is introduced. Independent Rust Ownership
Design is untriggered. No OAuth code or already adequate overflow proof needs
transplanting from another checkout.

Implementation proves the specification's observable falsifiers at these
existing boundaries, with particular attention to late-ready outbound
operation decisions; calendar-ineligible retained evidence; fresh/coalesced leeway;
replacement retention; joined failure sharing and next-call recovery;
unavailable-clock refusal; complete Duration rounding; and predispatch TTL
admission. Test cases, fixtures and exact commands belong to Implementation.
These are proof surfaces, not extra acceptance units or runtime infrastructure
requirements. Final assembled authorization-sensitive review remains required.

Version evidence: Cargo.lock resolves Moka 0.12.16, Tokio 1.53.1, HTTP 1.5.0
and redis-rs 1.7.1. Installed crate API docs and source were inspected:
Moka `future/entry_selector.rs`, `future/value_initializer.rs` and `policy.rs`;
HTTP `header/value.rs`. The published
[Moka entry documentation](https://docs.rs/moka/latest/moka/future/struct.OwnedKeyEntrySelector.html#method.or_insert_with_if)
currently identifies the same Moka version. Tokio's
[timeout contract](https://docs.rs/tokio/latest/tokio/time/fn.timeout_at.html)
documents immediately ready inner success despite a past deadline; installed
resolved source remains authoritative for implementation. No library upgrade
is needed. This is static design evidence, not runtime proof.

Reopen System Design for contradicted mechanism evidence, Specification for
changed observable behavior, and the root's separate Definition path for the
webhook horizon. Otherwise Planning can form the smallest implementation unit
from this ready design after its required independent review.
