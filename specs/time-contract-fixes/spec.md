# Time-contract corrections

Status: ready

Requester meaning: [Intent](intent.md). Source baseline:
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
The completed research and its independent review were supplied in the task
conversation; the source anchors below determine current behavior.

## Required behavior

### Outbound deadline

[Client::execute and exchange](../../crates/infra-outbound-http/src/lib.rs)
currently turn the remaining parent budget into a duration before constructing
a relative timeout. Suspension between those steps can extend the exchange.
One call must instead retain a fixed end, the earlier of the caller deadline
and the existing operation timeout measured from entry into execution. Admission,
setup instrumentation, transport, and complete buffered response collection
spend that same budget. A suspension must never restart it. At or after that
end, the client must refuse a new transport dispatch. The final operation
decision is made after the last await and complete buffered collection, before
terminal observation; success is allowed only if that decision occurs strictly
before the fixed end. An otherwise successful exchange decided at or after the
end becomes the existing `Error::Timeout`, including when an inner future is
immediately ready after expiry.

Terminal reporting records that fixed decision once, with the same result
returned to the caller. Synchronous observation callbacks may delay physical
return beyond the end; they neither restart the work budget nor reopen the
decision or dispatch. No deferred reporting task or new telemetry framework is
required. This contract bounds the operation decision, not physical return
through arbitrary synchronous callbacks, and does not promise rollback of an
already dispatched remote effect or an exact scheduler wakeup time.

### Introspection reuse

[IntrospectionVerifier::verify and Retention](../../crates/infra-bearerauthn/src/introspection.rs)
currently return retained principals without rechecking calendar time.
Reuse requires both the existing fixed monotonic retention and a current
calendar check: current time must be strictly before `exp`, with no expiry
leeway, and optional `nbf` must satisfy the existing 30-second leeway policy.
A forward clock step must not reuse an expired principal; a backward step must
not reuse a principal that is now too early. A calendar-ineligible retained
result follows the normal cache-miss/provider path, with no stale fallback.
Concurrent callers must retain same-token miss coalescing and must not erase a
newer eligible replacement while rejecting an older result.

Fresh provider verification, including shared fresh results for coalesced
waiters, retains the existing `exp` and `nbf` leeway and equality semantics in
[claims](../../crates/infra-bearerauthn/src/claims.rs). A fresh success inside
expiry leeway may serve that verification but cannot supply later cached
success at or beyond `exp`. Moka remains the owner of fixed, non-sliding
positive retention, size/capacity behavior, and miss coalescing. Provider
bulkheads, failure classifications, no negative cache, no remembered outage,
and cancellation behavior remain as documented in
[Authentication](../../docs/authentication.md#oidc-introspection).
Update its current wall-clock-step statement to distinguish monotonic retention
from the calendar eligibility now required at reuse.

### Unavailable authentication clock

[unix_now](../../crates/infra-bearerauthn/src/lib.rs) substitutes `u64::MAX`
before the Unix epoch; saturating lifetime arithmetic can consequently admit
an extreme-expiry token. When a usable Unix time cannot be obtained, JWT and
introspection verification, including cached reuse, must return canonical
unavailable trust (`Failure::Unavailable`), never authenticate using a sentinel
timestamp. Keep the existing sanitized transport mappings for unavailable
trust; no new public problem code or token-age cap. An internal closed reason
may identify the unavailable clock. Once usable time returns, ordinary
verification can resume; no permanent failure or successful result is cached
because of clock failure. Normal-time parsing, claim order, and lifetime
comparisons remain unchanged.

### HTTP Retry-After

[Problem rendering](../../crates/infra-http/src/problem.rs) floors fractional
seconds today. When a retry duration is supplied, emit integer delay-seconds
equal to the ceiling of that duration, with a minimum of one second. Thus
1999 ms emits `2`, zero emits `1`, and an exact positive whole second remains
unchanged. The complete `Duration` input range must render without arithmetic
panic, wrap, omission, or rounding below the requested delay; this includes a
fraction added to `u64::MAX` whole seconds. Preserve status, body, content type,
and absent-header behavior. This changes rounding of a lower-bound hint, not
the configured delay or the integer-seconds wire format.

### Redis cache TTL admission

[CacheNamespace::set](../../crates/infra-cache/src/lib.rs) currently asserts a
one-millisecond minimum and saturates oversized conversions to `u64::MAX`.
Feature-authored TTLs must be admitted only when their floored whole-millisecond
value is in Redis's positive signed 64-bit argument range. Reject out-of-range
input before dispatch rather than wrap, clamp, or silently substitute another
TTL. Technical Design owns the Rust API/error representation for that caller
contract. Ordinary admitted TTLs keep their existing millisecond flooring and
staleness semantics; no new maximum retention policy is introduced.

Redis owns its calendar time and absolute-expiry arithmetic. Argument admission
does not promise that server-time plus TTL is representable: a server refusal,
including absolute-expiry overflow, retains the existing unavailable-cache
outcome and no retry. Do not estimate server time from the host or claim a
successful cache write after refusal. Existing timeout ambiguity remains: a
timed-out write may have landed.

## Deliberately unchanged and deferred

| Research item | Disposition and reason |
| --- | --- |
| OAuth `expires_in` overflow | Already repaired on main: [into_token](../../crates/infra-oauth2-client-credentials/src/lib.rs) uses checked addition and `InvalidResponse`; [existing tests](../../crates/infra-oauth2-client-credentials/src/tests.rs) cover unrepresentable service and exchanged lifetimes before dispatch. Preserve these; do not transplant edits from the unrelated dirty checkout. |
| JWT fractional dates, equality, `iat` | Preserve current jsonwebtoken-compatible rounding/equality and the RFC 9068 future-`iat` rule. No new strict age or fraction policy. Introspection `exp`/`nbf` retain their unsigned-integer interpretation. |
| Webhook signed timestamps and raw HTTP fingerprints | Parsed timestamp canonicalization is not a demonstrated bug. Preserve signed body bytes, current timestamp signing semantics, and raw HTTP request fingerprint identity. |
| Messaging time representation | Preserve `time` 0.3.55, `UtcDateTime` event occurrence, raw HTTP timestamp identity, and normalized broker `stored_at` nanosecond hashes for DLQ/replay. No library/type migration. |
| Webhook receipt retention versus delivery horizon | Deferred user-owned policy. Current 14-day receipts can be shorter than 20 attempts with up to 24-hour Retry-After delays (19 days from those waits alone; about 20.059 days including other bounded waits, excluding queue delay/outage). Do not increase retention or reduce delivery policy in this core scope. Root must reconcile the pending desired duplicate-protection/replay horizon before final PR scope. |
| Global clocks, skew and operating budgets | Non-goals. Preserve all configured defaults, skew/leeway, budgets, synchronization policy, infrastructure, and delivery gates. |

## Composition and evidence boundary

A retained token whose wall clock advances beyond `exp` cannot authorize a
protected request from that retained result. It takes the provider path; an
outage produces existing unavailable trust, while genuinely fresh valid
evidence is evaluated with the unchanged fresh leeway. If calendar time is
unavailable, neither path authenticates. A bounded outbound call used by any
caller still cannot gain time through a scheduler pause. Retry hints and
cache TTL admission use their own existing units; neither changes those
authentication or outbound time policies.

The nearest falsifiers are dispatch or a successful operation decision at or
after the fixed outbound end (terminal reporting cannot revise that decision),
cached success after a forward-expiry or backward-`nbf` clock step,
authentication with a pre-epoch clock and extreme expiry, a rendered hint
shorter than its supplied duration, and an oversized TTL dispatched or clamped.
Implementation chooses deterministic cases and controls at the smallest
proving layer, reusing existing outbound deadline, introspection retention/
coalescing, problem-response, cache, and OAuth boundary coverage. Do not add a
general clock framework, duplicate already-adequate OAuth tests, or create
infrastructure solely for proof. Required repository validation and the final
assembled authorization-sensitive review remain owned by Implementation.

Technical Design selects the bounded mechanisms, cache representation and
invalidation behavior, clock-failure propagation, and cache input error shape.
No production code, test, manifest, build, or external effect belongs to this
Definition result. Reopen Definition only for a changed observable contract;
the deferred webhook business decision reopens its own dependent scope.
