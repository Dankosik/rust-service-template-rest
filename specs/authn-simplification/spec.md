# Contract-derived authentication simplification

Status: ready. Authority: [Intent](intent.md), [checked research](research/synthesis.md).

## Outcome and boundary

Every API operation enforces its declared authentication policy, including when
the idempotency profile is absent. Legitimate trusted providers work with mixed
JWKS and private HTTPS addresses. Library mechanisms replace redundant custom
parsing and transport while sealed verified identity, bounded work, deadlines
and protocol obligations remain intact. All ten review groups below are in
scope. This changes Stage 10.1 and its projections; the Stage 10.2 outbound
policy is deliberately unchanged.

## R1. One route contract

Build policy from method and matched route template in the same OpenAPI value
that supplies the complete served router. Global bearer security is the default;
an operation's explicit `security: []` is public. Missing operation security
inherits the global requirement. Unsupported/ambiguous security requirements or
contradictory exposure metadata fail composition/startup. Public probes ignore
Authorization and perform no auth-provider work.

Apply contract-driven auth to the fully assembled API router. A registered
handler absent from the document must never run: return sanitized Problem 500
`internal_error`. An unmatched path retains 404 and a method without a registered
handler retains 405. Implicit HEAD inherits GET policy unless an explicit HEAD
operation is registered; explicit HEAD has its own declared policy. Multiple
methods sharing a path are supported. With any effectively protected operation,
`mode = none` fails startup before traffic admission. A public-only service in
none mode makes no provider call. A retained enabled auth mode retains its
existing preparation semantics, even while the scaffold has only public probes.

Authentication precedes idempotency admission and handler extraction, and strips
the raw Authorization header after successful authentication. Only the verified
Principal crosses that boundary. Missing wiring/deadline evidence is sanitized
500, never anonymous success. Remove the per-handler protection obligation and
its single-method restriction; update affected composer and authoring guidance.

## R2. JWKS interoperability and key selection

A syntactically valid JWKS object with a keys array is processed per entry.
Unsupported types/curves/algorithms, encryption-only or incompatible use/key_ops,
missing required material and malformed individual entries are skipped with
bounded diagnostics. Unknown extension members do not invalidate usable keys.
Invalid top-level JSON/shape or a set with no usable keys fails preparation or
refresh. A failed refresh never replaces the last usable snapshot.

Bind each admitted public key to exactly one configured compatible algorithm
before token selection. An explicit JWK alg supplies that binding. Without alg,
admit the key only if its type/curve has exactly one configured compatible
algorithm; otherwise skip the ambiguous entry with a bounded reason. If entries
assign identical public material conflicting algorithms, neither assignment is
eligible. Token headers never choose a key's algorithm binding. Respect JWK
use/key_ops when present and select by the bound algorithm and optional kid.
If several eligible keys remain for that token,
reject it as ambiguous; never choose the first or try arbitrary alternatives.
Duplicate kid across distinct, algorithm-incompatible entries does not create
ambiguity for a token with one eligible key. No kid is permitted only when one
eligible key remains. Missing kid match can request the existing bounded refresh;
ambiguous matches are invalid tokens and do not create refresh storms.

## R3. Operator diagnostics, safe caller errors

Startup errors distinguish invalid configuration/client preparation, discovery
fetch or parse, configured/discovered issuer mismatch, JWKS fetch/parse, and no
usable keys. Operators see the phase, bounded reason and safe configured
issuer/audience/endpoint context; mismatch identifies both safe issuer values.
Never print tokens, credentials, raw response bodies, raw key material, or
unfiltered provider errors. Render discovered values only after bounded,
credential-free sanitization; unknown unsafe text becomes a reason.

Runtime verification and refresh metrics/logs use closed reason enums, never
token/kid/subject/issuer/URL as metric labels. A refresh failure logs warn and
counts its bounded reason while keeping usable old keys. Caller Problem details
remain fixed and do not expose diagnostic reasons or provider details.

## R4. JWT trust and typed claims

Retain jsonwebtoken 11.1.0 with aws-lc. Use decode_header for local selection,
from_jwk for key construction, and typed serde claims through verified decode
with Validation for signature, issuer, audience, exp and optional nbf. Explicitly
require iss/aud/exp, exact configured issuer, any configured audience, 30 seconds
of skew, and nbf checking. General resource-server tokens need a nonempty sub or
coherent client identity; client_id/azp/appid/cid aliases must agree when present.
The RFC 9068 profile additionally requires access-token typ (at+jwt or
application/at+jwt, case-insensitive), sub, client_id itself, jti and iat, and
rejects iat later than now plus 30 seconds. The generic profile does not invent
mandatory RFC 9068 claims or typ.

Consumed claims remain typed: issuer/identities are strings, aud is a string or
string array, epoch claims are nonnegative integral seconds. Required null/empty
claims reject. Optional null otherwise has the normal serde absent meaning,
except for nbf: omission is allowed, but present `nbf: null` is invalid token
evidence and returns 401, following jsonwebtoken Validation with nbf checking
enabled. No preprocessing turns numeric null into omission. Reject
duplicate consumed fields through typed deserialization; unrelated extension
fields are ignored. Unknown critical JOSE extensions reject under RFC 7515;
token-supplied jku/x5u/jwk never select trust material or trigger HTTP. A Principal
is constructed only after all signature and applicable claim/profile checks.
No second generic JSON visitor framework or signature-only validation survives.

## R5. Trusted authentication transport

Use a reusable pooled reqwest Client, HTTPS-only with normal certificate and
hostname verification, no redirects, no ambient proxy and explicit no retry.
Configured private HTTPS IdPs and validated discovery JWKS endpoints are allowed.
The operator controls issuer/introspection endpoint; only the HTTPS discovery
response whose issuer exactly matches configuration supplies JWKS URL.
Bearer input cannot affect destinations. Client credentials apply only to the
configured introspection request. The existing 1 MiB response ceiling, 3-second
provider attempt cap, 6-second JWT startup cap and 100ms response reserve remain.
No response is accepted merely because Content-Length was small.

Remove auth's custom DNS path and infra-egress-dns dependency. Preserve
infra-egress-dns and post-resolution address admission for independent outbound
HTTP and any untrusted destination use. Assess/document the distinction instead
of applying this simplification globally. No new private-network switch or TLS
verification bypass is introduced. Pooling adds no periodic readiness probe:
provider outages retain the existing request-local readiness consequence;
initial JWT trust still precedes admission. Close auth background ownership
through the existing process shutdown path; do not claim direct joining of
reqwest internal tasks beyond its documented drop/runtime ownership.

## R6. Verified scopes and expiry

Principal and the HTTP verified wrapper expose `scopes()` and `expires_at()`;
expiry is the verified exp in epoch seconds, without extending it by clock skew.
Scopes come from space-separated scope or a scp string array, retain exact case,
and are deduplicated. Absent/null scope evidence means an empty set. If both
forms are supplied their normalized sets must agree; disagreement rejects,
avoiding accidental privilege union. Malformed types/invalid scope tokens reject
as token evidence for JWT and provider evidence for introspection. Scope tokens
follow RFC 6749's scope-token character grammar; empty overall scope is empty.
The small optional `require_scope` helper is included: a verified principal
missing the named scope yields Problem 403 `forbidden` and
`Bearer error="insufficient_scope"`. It adds no role framework, automatic route
scope policy or scope names to metric labels.

## R7. Introspection classification and capacity

Provide an operator-enabled introspection result cache, disabled by default.
With caching disabled, retain one client-secret-basic RFC 7662 POST per admitted
opaque token, proper credential component encoding and no retry. Enabling the
cache permits a valid hit to avoid that provider call; a miss or expired entry
uses the same existing introspection path and failure/deadline rules.

Cache only successfully verified active results, isolated by the exact opaque
token and the issuer, accepted audiences and provider credential/endpoint trust
context used to verify them. A result from another token or trust context must
never authorize a request. Never cache inactive results, invalid tokens,
incomplete/malformed responses, provider errors or timeouts. A hit supplies only
the verified identity/scope evidence and must still satisfy the current token
temporal rules; it neither extends token validity nor bypasses request deadline
or bearer-envelope checks.

Storage has finite capacity and entries have a finite configured maximum
lifetime, fixed when successful verification completes. Cache hits do not renew
that lifetime. An entry stops being usable at the earlier of that lifetime's
end and token exp, without the 30-second token-validation leeway. A result
already at or past exp may still be handled by the unchanged uncached temporal
policy, but is not cacheable. At capacity, storage remains bounded without
weakening verification; the particular admission/eviction mechanism belongs to
Technical Design. Expired entries are misses, including during provider failure:
there is no stale-result fallback. Documentation must state that enabled caching
can delay detection of revocation until the valid entry expires. Token and
cached identity confidentiality retain the existing secret/logging constraints.

An active response missing iss/aud/exp or both subject and client identity
rejects with 401 invalid_token and internal reason `missing_claim`. Wrong issuer,
audience, expiry/not-before or inactive token is also 401. Active responses with
wrong claim types, malformed JSON or unusable provider envelope remain 503;
`active=false` ignores other claim meaning. In an active response, nbf may be
omitted, but present `nbf: null` is wrongly typed provider evidence and returns
503, consistently with other supplied invalid numeric claims. Duplicate consumed
fields reject.

Transport errors, non-200 responses including provider 5xx, provider's own
3-second cap, malformed response and local provider capacity exhaustion yield
503 `authentication_unavailable`. Use configurable nonzero provider concurrency,
default 32, with immediate rejection and no queue. A request that exhausts its
own available deadline/reserve yields explicit 504 `request_timeout`; it must
not sleep or stay pending to force outer timeout. Deadline classification uses
which budget was exhausted, not all reqwest timeout errors indiscriminately.

## R8. Shared refresh lifecycle

Expose current keys as immutable watch/Arc snapshots without awaiting a global
mutex on known-key verification. Keep one process-owned refresh at a time,
15-minute periodic refresh and 30-second unknown-kid cooldown. Its 3-second
attempt budget belongs to the refresh worker, independent of the triggering
request. Each waiter waits only within its own request deadline less response
reserve and may leave without cancelling work another waiter needs. The process
cancellation token cancels the worker; bootstrap joins it in its existing
background budget. No request spawns untracked long-lived work.

After a successful refresh or success cooldown, a still-unknown key is 401.
Refresh failure/failure cooldown with no matching cached key is 503. A known
eligible cached key remains usable after refresh failure; token exp is still
validated. Ambiguity is 401. A waiter's exhausted budget is 504 even when a later
waiter succeeds from the same shared refresh. Refresh remains rotation support,
not immediate token/key revocation.

## R9. Configuration and crypto

Auth config is a serde internally tagged enum with variant-specific fields and
deny_unknown_fields. None is the omitted default and accepts no dormant provider
fields. JWT requires issuer/audience and supports token_profile (default
resource-server) and algorithms (default [RS256]); introspection requires
issuer/audience/endpoint/client id and environment-only secret plus concurrency
and the default-off cache's capacity/lifetime controls.
Unknown/foreign fields fail with a useful key/mode diagnostic. Existing `audience`
string input remains accepted; the same field also accepts a nonempty list of
nonblank exact audiences. Duplicate list entries are harmless normalization;
whitespace is not silently trimmed into a different audience.

Supported configured algorithm values are RS256, ES256, PS256 and EdDSA only;
the list must be nonempty. Key families are RSA for RS256/PS256, EC P-256 for
ES256 and OKP Ed25519 for EdDSA. No symmetric/none algorithm is accepted. Keep
aws-lc consistent with TLS and prevent accidental feature unification from
causing jsonwebtoken's dual-provider auto-selection panic.

One validated URL type/grammar owns HTTPS, host, whitespace/control, userinfo,
query and fragment policy for configured and discovered endpoints. Retain the
existing no-userinfo/query/fragment grammar and exact issuer identity, without
duplicated parser implementations. Adopter guidance calls out rejection of
previously dormant foreign fields under none and adding algorithms/audiences;
no deployed consumer migration or compatibility shim is invented.

## R10. Bearer envelope, projections and development policy

| Request at a protected operation | Response |
| --- | --- |
| Missing Authorization or one well-formed unsupported scheme | 401 authentication_required; bare Bearer challenge |
| Duplicate Authorization, malformed Bearer syntax or invalid envelope bytes | 400 authentication_malformed; Bearer error="invalid_request" |
| Invalid JWT/introspection token evidence | 401 authentication_invalid; Bearer error="invalid_token" |
| Token exceeds bound | 431 authentication_oversize if middleware sees it; native inbound header 431 can precede routing |

Keep the operator's configurable inbound aggregate header bound. Use the smaller
of the existing 32 KiB token ceiling and configured max_header_bytes as auth's
token ceiling; the aggregate header framing may reject earlier. Do not raise the
header cap to make the larger token bound reachable. The bearer parser enforces
the bound before decoding/copying token contents. Public routes remain unaffected
by auth envelope parsing, subject to shared native inbound limits.

Reduce initializer markers by removing whole engine files where appropriate;
retain supported profile selection and no disabled-engine dependencies. Remove
auth DNS-only/copy-of-production-rule tests with the removed auth policy; retain
production outbound DNS tests. Update authentication, architecture/decision,
configuration, authoring and template documentation to the resulting behavior.
Existing generated OpenAPI remains derived from annotations/composition.

Development policy: meet normative MUST/MUST NOT requirements. Follow SHOULD
guidance unless a documented, understood reason justifies deviation. Extra
strictness beyond RFC/reference behavior must identify a concrete threat crossing
an untrusted boundary, a real incident, or a necessary accepted application
constraint, with cost and compatibility consequences. Suspicion or strictness
alone is insufficient. Library reuse never excuses lost normative checks.

## Proof and completion boundary

Reuse existing tests and add only missing material behavior proof. The executor
selects cases/commands; this is not a test-plan gate. Cover bypass prevention,
full bootstrap without idempotency, native 404/405/HEAD, mixed/ambiguous JWKS,
actual library/crypto validation, response classification, two refresh waiters
with different deadlines, private trusted HTTPS and provider bounds, scope
authorization, config rejection, initializer projections and default-off versus
enabled introspection caching with bounded capacity/retention, expiry and
trust-context isolation. Existing TLS fixture
authority suffices; no live IdP, new database environment or performance target
is introduced. Preserve all applicable repository/CI gates and factor expensive
runtime validation from harness/profile transformation checks. Final assembled
authorization/lifecycle changes require fresh independent delivery review.

Technical Design is required for contract composition, owner boundaries,
configuration representation and refresh lifecycle. Reopen Specification only
if those decisions expose a material behavioral contradiction; user technical
confirmation is not a prerequisite. PR publication/CI remains with the root
delivery owner; merge/deployment are outside this request.
