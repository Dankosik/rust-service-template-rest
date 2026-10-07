# Outbound machine authentication decisions

<!-- template:begin outbound-auth:docs-outbound-machine-authentication-decisions -->
Stage 10.8 library and lifecycle decisions, recorded 2026-09-26; client
authentication and token exchange (D1–D3a, D7 below) were revised 2026-09-29
from research recorded against `origin/main` 3c220db. The [research
synthesis](https://github.com/Dankosik/rust-service-template-rest/blob/e4be7060311f3d33486b437b9bfb033db204708c/specs/service-to-service-auth/research/synthesis.md)
keeps the claim-level evidence; this record keeps only the accepted decisions
and their reopen conditions.
[Guide](outbound-machine-authentication.md) owns adoption and observable
behavior.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| D1: private-key client assertion (`private_key_jwt`, RFC 7523) only; no client-secret mode kept beside it | RFC 9700 §2.5 recommends asymmetric client authentication so the authorization server holds no shared secret; every shortlisted server supports a key-based method. A shared secret is the same credential class as the static-bearer and HS256 schemes this work replaces. | Amazon Cognito and other secret-only providers cannot use this profile. Reopen when a required provider supports only shared secrets. |
| D1: one request form for both grants — `grant_type`, `client_id`, `client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`, `client_assertion`, plus `scope`/`audience` when configured (RFC 7523 §2.2, RFC 7521 §4.2) | Keycloak, Hydra, Authelia, Spring Authorization Server, Duende, Okta and Auth0 (Enterprise) accept this form; one registered client and key cover both client credentials and token exchange. | Zitadel does not fit: its service users need the JWT-bearer grant, and only a second OIDC application with its own key can call token exchange. Reopen if Zitadel is chosen — the gap is the JWT-bearer grant form plus a separate exchange identity. |
| D2: assertion claims `iss=sub=client_id`, single-string `aud` = the authorization server's issuer identifier (not its token endpoint), `iat=nbf=signing time-10s`, `exp=iat+60s` (50s after signing), a fresh `jti` per request; header `alg`, `kid`, `typ: client-authentication+jwt` | rfc7523bis §4 and the 2025-01-24 OIDF disclosure (CVE-2025-27370/27371) require a single-string audience naming the issuer, so a malicious server cannot obtain an assertion another server accepts; Keycloak caps assertion age at 60 s and requires a single-use `jti`, as do Hydra, Authelia, Duende, Auth0 and Entra. | A server accepting only its token endpoint (Hydra v26, Okta, Entra) gets that URL instead — safe because each integration is configured against one authorization server. Reopen if a required server rejects the typed `typ` header (Spring Authorization Server 1.5.x on Spring Security 6.5 does). |
| D3/D3a: RFC 8693 token exchange authenticated with the same client assertion, cached per credential owner keyed by SHA-256 of the subject token, with best-effort settled targets of `exchange_cache_capacity` entries (default 1024) and 16 MiB Bearer-token bytes; concurrent misses per subject coalesce, and a resource 401 evicts only that subject's entry; no `actor_token` | Keycloak 26.7.4 standard token exchange V2 (GA since 26.2) authenticates the requester as a confidential client and requires it in the subject token's `aud`; Spring's `TokenExchangeOAuth2AuthorizedClientProvider` also reuses an exchanged token until it expires. Huskarl is a maintained Rust alternative with token exchange, but the selected adapter is retained because its policy still owns admission, transport/error projection, no-retry behavior and lifecycle. | Count and payload are retention targets, not strict admission or RSS bounds: concurrent inserts, active references, keys, metadata and allocator overhead remain. Zitadel emits `act` only for an `actor_token` under its impersonation permission, and authentik authenticates exchange only with a client secret — neither fits D1. Reopen if a required server emits `act` under D1's constraints. |
| One cached token behind a double-checked refresh lock, as Go's `oauth2.ReuseTokenSource` and yup-oauth2 do | Moka's one-key `try_get_with`/`Expiry` cache shared failures, but its retention depended on initializer internals, a zero-lifetime trick, and a second clock that Tokio test time cannot move. | A completed service acquisition failure is shared for one second; then one later caller may retry under its own deadline. Reusable tokens win over suppression. Exchange failures remain coalesced only for concurrent waiters, with no persisted negative cache. |
| One driver-owned refresh once at most five minutes, or a quarter, of reuse remains, as Azure.Core's bearer policy refreshes early without blocking callers | Refreshing only at the cutoff made every concurrent caller wait for the provider. On a DigitalOcean c-4 with 64 concurrent callers and a 100 ms provider, each refresh held 64 requests for over 20 ms; with the early refresh only the first acquisition does. An inline early refresh would spend one caller's deadline on the provider. | `Credentials::prepare` returns a `RefreshDriver` that its integration drives and awaits before dependency drop. Its scheduled five-second cap includes lock waiting; final owner loss or lifecycle shutdown cancels it. A completed failure is shared for one second, success clears it, and the next refresh waits thirty seconds. |
| A resource 401 evicts only a token at least thirty seconds old | Evicting on every 401, as Spring Security does, turned a resource that refuses every token (wrong audience, clock skew) into one token request per call; never evicting, as Go's `oauth2` does, keeps a revoked token until it expires. The provider answers a request made seconds after the last with an equivalent token, so nothing is lost by keeping a young one. | A token revoked within thirty seconds of issue is used until that age. Reopen if a provider revokes tokens that young or rate limits one request per thirty seconds. |
| A provider rejection keeps its registered `error` code as a closed enum; a 429 is `Unavailable` | One `Rejected` reason hid whether the key, the scope, or the grant was refused, and the adapter emitted no log. RFC 6749 section 5.2 and RFC 8693 section 2.2.2 register a finite code set, so mapping it leaks no provider bytes; Go's `oauth2.RetrieveError` exposes the same code. A throttled request may succeed unchanged later, like a 5xx. | An unregistered code is `Other`; `error_description` is still discarded. `Retry-After` is not read, since the adapter does not retry. Reopen if a provider's diagnosis needs `error_description`. |
| The assertion is dated ten seconds back | Go's `oauth2/jws` does the same for hosts whose clock runs ahead of the provider's. `iat=nbf=now` made a provider one second behind read the assertion as not yet valid. | The assertion is usable for fifty seconds after signing instead of sixty; each is used once, immediately. |
| `algorithm` is required, with no default; `EdDSA` is not offered | The algorithm must match the key, so a default only chose `RS256` for whoever omitted it, and FAPI 2.0 admits no PKCS#1 v1.5 signatures. RFC 9864 (2025) deprecates the polymorphic `EdDSA` identifier for `Ed25519`, which `jsonwebtoken` 11.1.0 cannot emit. | A tuple written without `algorithm` fails startup naming the key. Reopen `Ed25519` when `jsonwebtoken` and the chosen authorization server accept it. |
| A bound client may require `OnBehalfOf` (`require_on_behalf_of()`), refusing a request without it before any I/O | The subject travels in request extensions, the only channel a `tower::Service` has, so a forgotten one was a call made with the service's own, usually wider, authority. A second client type per transport would duplicate both bindings for one boolean. | Opt-in per binding: a client that serves both paths keeps the fallback. Reopen if an integration needs the requirement per call. |
| `exchange_cache_capacity` is a configuration key (default 1024, inclusive 1–65536) paired with a 16 MiB payload retention target | The count is users active on one replica within a token lifetime; the payload target prevents unusually large Bearer values from turning a count-only cache into unbounded retention. | Both are best-effort settled retention targets rather than instant admission control or an RSS guarantee. Reopen if a service needs a strict memory budget or a different per-owner cache target. |
| `provider_concurrency` is a positive `u32` configuration key (default 32) | One semaphore on the prepared `Inner` bounds actual attempts across both grants; cache hits and same-key waiters consume no permit. The default matches introspection, while operators choose their deployment's bound. | Add the field to direct `Options` literals and handle `AtCapacity` in exhaustive matches. No admission queue, retry, reservation, priority, or cached capacity failure. Independent preparations have independent bounds. |

| The gRPC binding answers its two local refusals, a caller-supplied `Authorization` and a missing required subject, with `INTERNAL` | Both are composition mistakes of this service. gRFC A54 reserves `INVALID_ARGUMENT`, `FAILED_PRECONDITION` and five more codes for the application and has a channel turn them into `INTERNAL` when call credentials fail an RPC; the earlier `INVALID_ARGUMENT` blamed the inbound caller whenever a handler forwarded the status. | A handler that matched `INVALID_ARGUMENT` from this client now sees `INTERNAL`. The HTTP binding keeps its typed `Error` variants. |
| `expires_in` is admitted only as a JSON number of whole seconds | RFC 6749 section 5.1 defines a number, and every supported provider in the guide sends one. Go's `oauth2` also accepts a numeric string for older Microsoft endpoints, which this profile does not support for another reason (`x5t#S256`). | A provider that sends a string is an `invalid` outcome on every acquisition, visible at once. Reopen when a provider the guide lists as supported sends a string. |
| One new provider crate, independent of inbound authentication | Extending inbound auth joins separate trust and credential lifetimes; placing OAuth in outbound HTTP makes an optional protocol a dependency of every bare HTTP consumer. | Explicit crate/profile pruning keeps independent adoption; remove speculative traits and unused registry/generator paths. |

Authorization-server evidence: versions checked were Zitadel v4.19.2,
Keycloak 26.7.4, Hydra v26.2.0, authentik 2026.8.3, Authelia v4.39.28, Spring
Authorization Server 7.1.1, Duende 8.0.9, plus Auth0, Okta, Entra and Cognito
documentation (2026-09-29). The [service-to-service authentication
guide](service-to-service-authentication.md#choosing-an-authorization-server)
carries the shortlist, the not-shortlisted list with reasons, and Keycloak
registration notes for this path.

## Crate choices and named custom gaps

| Need | Options examined | Decision |
| --- | --- | --- |
| Token requests (two grant forms) | `oauth2` 5.0.0 (2025-01-21, MIT OR Apache-2.0): `AuthType` is only `BasicAuth`/`RequestBody`; `private_key_jwt` is a FIXME at `src/endpoint.rs:110`; every builder hard-codes `grant_type`; no token-exchange or JWT-bearer request and no generic grant; the maintainer keeps JWT signing and DPoP out of the crate (#211, #265). `openidconnect` 4.0.1 (2025-07-06): neither feature, and its signing uses RustCrypto `rsa` (RUSTSEC-2023-0071). | Remove `oauth2`; one template-owned form POST (see below). |
| Assertion signing | `jsonwebtoken` 11.1.0 (already locked, aws-lc backend): `encode` with `Header { typ, kid }`, RS/PS/ES; `use_pem` adds `simple_asn1` and `pem` 3.0.6 (a reported duplicate beside rcgen's `pem` 4.0.0; `num-bigint` and `time` already locked) and reads PKCS#1/PKCS#8 RSA and PKCS#8 EC. `josekit` (OpenSSL), `jwt-simple` (second crypto family), `biscuit`/`aliri`/`openid` (`ring`), and `jose-jws`/`jwt-compact` (stale) were rejected. | `jsonwebtoken::encode` with `use_pem`. Reopen the duplicate `pem` when `jsonwebtoken` moves to `pem` 4. |
| Token exchange client | `oauth2` 5.0.0 lacks a same-level private-key assertion plus RFC 8693 flow. Huskarl 0.11.4/core 0.10.5 is a maintained alternative with token exchange, private-key JWT and DPoP; its custom HTTP/signer seams can use the admitted transport/backend, but its defaults and extension points still leave the template's assertion, audience, monotonic admission, closed-error, no-retry, retention and lifecycle policy local. | Retain the template-owned request on the shared form POST for this closeout. This is a dated local-fit decision, not an absence claim; reopen when a required grant/DPoP rollout or upstream change materially reduces those retained policy costs. |
| Exchanged-token cache | Moka 0.12.16 (2026-08-09, MIT OR Apache-2.0 plus Apache-2.0; already locked and used by the introspection cache with SHA-256 keys, per-entry expiry and coalescing). | Moka `future::Cache` with `Expiry`, bounded by `exchange_cache_capacity`. |
| Assertion `jti` | `uuid` (workspace), aws-lc random. | `uuid` v4. |
| Absorbed-failure log | `tracing` (workspace; the workspace's event facade). | `tracing`, one `WARN` event. |
| Real-server proof | Compose service in `env/docker-compose.yml` (needs the `integration` profile, which an OAuth-only service does not retain); `testcontainers` (new dependency, Docker driven from test code); a pinned `docker run` in the proof script. | Pinned `docker run` of Keycloak 26.8.0 by digest; the pin is bumped by hand because Dependabot does not read the script. |
| Inbound `act` | Extends the existing borrowed-claims parser. | Template-owned claim model, not a mechanism. |
| DPoP | Huskarl provides DPoP support, but adopting it would still require a separate accepted sender-constraint rollout and compatibility/proof work. | Deferred; see below. |

**Why `oauth2` goes although it could carry the assertion.** Keeping it for
`client_credentials` with `add_extra_param` was previously accepted because
hand-written code would duplicate "Basic encoding, form construction, and
standard response handling." That rationale no longer holds: Basic encoding
disappears with the secret, and form construction and response parsing must
be template-owned anyway for token exchange, which `oauth2` cannot send.
Keeping the crate would add a second request path (its `AsyncHttpClient`
adapter plus its error mapping) beside the exchange path, and keep the
`thiserror` 1.x duplicate, without removing any template code. One form POST
over the existing bounded client, with `url::form_urlencoded` and one serde
response type, is the fewer-mechanism design. Reopen if a maintained crate
offers both a client-assertion hook and RFC 8693 requests.

The template-owned gaps are: the assertion claims and signing call; the two
grant forms and their response type; the exchanged-token cache wiring; the
`act` claim model; configuration and initializer integration; sanitized
outcomes. There is no custom protocol serializer/parser beyond one serde
response type, no flight state machine, no retry loop, and no general
token-source abstraction.

## Deferred with reopen conditions (D7)

| Item | Decision | Reopen |
| --- | --- | --- |
| DPoP (RFC 9449) | Not adopted. Audience-bound tokens of minutes lifetime on a private network; per-request proof signing, nonce state and retry behavior on token and resource calls. Huskarl is a maintained Rust option, but no migration or rollout is selected here. Keycloak supports it since 26.4; Zitadel, Hydra and authentik do not. | Tokens leave the private network, a compliance regime requires sender constraint, or the chosen authorization server and a maintained Rust client support DPoP for client credentials. |
| mTLS-bound tokens (RFC 8705), SPIFFE, WIMSE | Not adopted. No per-service certificates or mesh on the target platform; WIMSE drafts split in late 2025 and have no mainstream implementation. | The platform issues workload identity. |
| Platform-issued client assertion (workload identity federation) | Not adopted: the assertion is signed with a configured private key. Keycloak 26.6 supports federated client authentication, accepting a Kubernetes service-account or OIDC identity-provider token as `client_assertion` (SPIFFE JWT-SVID stays preview), which leaves the service no long-lived key. Railway issues no workload token to a running service (checked 2026-10-01). | The platform issues a workload token the chosen authorization server accepts as a client assertion. |
| `act` from Keycloak | Not relied on: Keycloak token exchange delegation, which emits `act` for the acting client, is experimental in 26.7 and preview in 26.8 (checked 2026-10-01). The inbound verifier already reads `act`, and the exchange request needs no `actor_token` for it. | Delegation becomes a supported Keycloak feature; then record the registration it needs in the guide. |
| Transaction Tokens | Not adopted: draft-ietf-oauth-transaction-tokens-11 (2026-07-30) is not an RFC, no shortlisted authorization server issues them, and a Txn-Token is not an access token. | Published and a chosen authorization server issues them. |
| Private JWK key input | PEM only. | An authorization server hands out keys only as JWK. |
| Introspection client secret (inbound opaque-token mode) | Unchanged; this is a different mechanism from outbound client authentication — [Service-to-service authentication](service-to-service-authentication.md) requires JWT access tokens and `oidc-jwt` for this path. | Introspection becomes part of a service-to-service path. |

## Resolved dependency graph

The lock removes `oauth2`, `thiserror` 1.0.69 and `thiserror-impl` 1.0.69; the
`thiserror` 2.x family remains. It adds `simple_asn1` 0.6.4 and `pem` 3.0.6
through `jsonwebtoken`'s `use_pem` feature; `pem` 3.0.6 duplicates rcgen's
`pem` 4.0.0, which `cargo deny` reports as a warning. `url` no longer carries
the `serde` feature OAuth used to enable. Advisory/license gates and executable
profile proof remain CI-owned; these graph observations do not claim their
success.

## Cache, budgets, and finality

`Credentials::prepare(Options)` returns one externally cloneable credential
owner and a non-cloneable `RefreshDriver`. The driver holds only the private
work state, so it cannot retain the final external owner. It runs with the
integration's existing shutdown future and its completion is awaited in the
existing background-join phase before dependencies drop. Final external-owner
loss also completes it; dropping the driver while a client survives terminally
closes that client. A fresh call then reports `Timeout` for an elapsed deadline
or `Unavailable` otherwise, before cache or I/O.

One service-token acquisition runs at a time. A reusable cache hit returns
without waiting and takes precedence over a completed-failure record; otherwise
each caller waits under its own deadline, rechecks, then fetches under the
smaller of that deadline and the five-second cap. Completed provider refusal,
transport, response-limit, unavailable/invalid-response, assertion failure and
a full five-second adapter timeout are shared for one second, then a later
acquisition may recover; success clears the record. Cancellation, lock waiting,
and a shorter caller deadline do not publish it. Expiry or eligible 401 eviction
does not clear it. The driver owns refresh-ahead. Its five-second budget begins
when scheduled and includes lock waiting, and owner loss or lifecycle shutdown
cancels it without admitting a following attempt.

The private `post_form(&self, fields, deadline)` uses the fixed endpoint, the
owner's bounded token client and the absolute attempt deadline; it sends `application/x-www-form-urlencoded`
(`url::form_urlencoded::Serializer`) with `Accept: application/json` for
either grant and decodes one private serde `TokenResponse`. A 5xx or 429 token
response is `Unavailable`; any other non-2xx is `Rejected`.

`Token` holds the private sensitive header and an optional Tokio monotonic reuse
cutoff; one clock governs every expiry decision. A representable positive expiry
is `acquisition_start + expires_in`; zero, already elapsed, and unrepresentable
expiry are invalid. The reuse cutoff is `expiry - 10s`; a token already inside
that margin serves only the request that fetched it. Omitted expiry is likewise
request-only and never cached. A reused token is always before its cutoff, so
dispatch needs no second expiry check. There is no fallback to a prior token.

A resource 401 evicts the token that request used when its token request
started at least thirty seconds ago, removing it only while it is
still the cached value (`Arc::ptr_eq`), so a concurrently acquired replacement
survives. The response is returned without replay; the next operation acquires
anew. This follows Spring Security's authorization-failure handler rather than
Go's keep-until-expiry, so a revoked or rotated token does not fail every call
until its provider lifetime ends. A 403 is a permission result and keeps the
token.
The ten-second rule is a refresh preference, never a minimum accepted token TTL.

Exchanged tokens use the same `Token` and cutoff in a Moka cache keyed by the
subject token's SHA-256 digest. It retains a settled best effort of both the
configured entry count and 16 MiB of Bearer-token bytes through weighted
entries. This does not claim strict memory admission or RSS control: eviction
can lag concurrent inserts, active calls retain values, and keys, metadata and
allocator overhead remain. A Moka hit still needs the Tokio-clock reuse check;
a stale hit is removed and exchanged once more. A fresh short-lived token serves
the request that fetched it; omitted expiry never stores it. The shared exchange
belongs to the first caller's future and its deadline; if that caller cancels,
a waiter starts its own. Exchange failures are coalesced only for concurrent
waiters and are never persisted as a negative cache. Exchanges for different
subjects acquire the same owner's finite provider capacity as service tokens
and refresh-ahead. Inbound admission and cache-entry capacity are separate controls.

Each actual fetch initializer creates its existing attempt metric, checks its
original operation context cutoff, then uses `Semaphore::try_acquire` before assertion signing.
An expired deadline yields `Timeout`; saturation yields `AtCapacity`. The
borrowed permit belongs to that initializer through the complete token body,
JSON parsing, and token admission. RAII releases it on success, every error,
timeout, and future drop. Waiters and cache hits hold no permit. Cancellation
of a waiter cannot free its leader's slot; a replacement leader undergoes
admission anew. The semaphore is never closed or manually replenished.
Refresh refusal keeps the usable token and retry spacing of thirty seconds plus
an independently sampled zero-to-three-second spread. Eligible service-token
refreshes similarly advance their start by a sampled share of at most one tenth
of their lead, capped at thirty seconds; exchange and request-only tokens never
schedule background refresh.
No detached replacement request or resource dispatch follows a refusal.

The configured positive u32 is checked for `usize` conversion and
`Semaphore::MAX_PERMITS` before construction; an unsupported target receives a
sanitized configuration error rather than a semaphore panic. All positive u32
values fit the current 64-bit targets. One owner at the default admits at most
32 active bodies with a 1 MiB payload ceiling each, with at most 32 MiB requested
accumulator storage between growths or 64 MiB during simultaneous relocation.
Transport frames/buffers, allocator overhead, parsing, and cached tokens are
additional costs; this is no process-memory or throughput guarantee.


The caller's resource context is retained through acquisition and forwarded
unchanged to resource dispatch; the resource ceiling is fixed before token work.
The token client uses constants: five seconds, 64 response headers, 1 MiB encoded body. One MiB matches the existing provider envelope and
allows provider extras without a token-size policy; 64 counts metadata, because
the shared client exposes a header count and no configurable header-byte limit. No claims about measured
latency, memory, or provider capacity are made by these bounds.

## Ownership and proving surfaces

Typed configuration owns key presence, RFC scope representation, safe diagnostic
context, and fixed-endpoint syntax; adapter construction independently admits
direct options by the same rules, including that a whitespace-only value is
empty, the exchange-cache range, and the refusal of an `@` in the endpoint's
raw authority, which `Url` parses away when the userinfo is empty or follows a
backslash. Neither owns the other's dependencies: config uses `url`, while
the adapter receives primitives/SecretString through composition and does not
depend on service-config. Normal config validation runs in every existing binary;
there is no eager token call or extra service lifecycle field.

The section decodes with derived serde like every other section. Its values
stay out of decode failures through the loader's one redaction rule: `load`
rebuilds a failure at a key a variable sets from the key and the expected
form, and `integrations` is listed in `VALUE_FREE_SECTIONS` there, so a file
value of this section is not shown either. The section first shipped with an
explicit decoder over `config::Value` for that purpose; once the loader had
the rule for every variable, a second decoder was a parallel path and was
removed. With it went its refusal of a number where text is expected:
config-rs converts scalars here as in every section, and validation still
checks the result. `algorithm` is decoded by hand to answer a refused
value with the accepted ones. `provider_concurrency` has a field-local
untagged typed/text scalar decoder inside the OAuth markers. It preserves the
scalar kind through config-rs, whose ordinary unsigned conversion rounds
floating-point values; fractions, booleans, negatives, and values outside u32
are rejected, and validation refuses zero. The loader retains value-free
errors. No optional introspection-profile decoder dependency is introduced.

Runtime errors separate caller Authorization conflict, a missing required
subject, acquisition failure, and existing resource transport failure. Acquisition reasons and all public
Debug/Display are closed. Record
`oauth2_token_acquisitions_total{grant, outcome}` once per actual initializer,
with `grant` in `client_credentials | token_exchange` and finite
success/timeout/capacity/transport/limit/unavailable/rejected/invalid/cancelled/assertion
outcomes, plus the seven registered error codes in place of `rejected`. A
capacity refusal is recorded once per initializer, emits no outbound HTTP
attempt, and is not multiplied by coalesced waiters. A failed background refresh, which no caller receives, logs
`oauth2_background_refresh_failed`. No scope/audience/URL/integration label or response content
is emitted. Existing resource transport error policy remains unchanged.

The production adapter's local token/resource-server proof covers encoding,
audience and scope omission, permissive RFC success parsing, shared success and
one-second completed service-failure suppression, reuse cutoff, missing expiry,
cancellation replacement, per-waiter
budget, owner isolation, Bearer injection, 401 eviction that spares a newer
token, and 401/403 without replay. It also verifies the assertion
header and claims with the matching public key, distinct `jti` values, key and
algorithm refusal, both request forms, the issued-token-type check, per-subject
reuse and coalescing, a waiter restarting the exchange its first caller ran
out of budget for, single-subject eviction, uncached exchange failures, one
exchange for a short-lived token, one count per token request under its grant
and outcome (success, a registered error code, timeout, cancellation) read
from a local Prometheus recorder, and gRPC on-behalf dispatch. The Keycloak
suite (`integration` feature, `make test-integration-oauth`, CI surface
`oauth_integration`) proves against a real server what the fixture assumes:
the three algorithms, both grants, and the reported error codes. It lives in
the crate's unit-test module because only the private `cfg(test)` constructor
admits a loopback HTTP token endpoint. Reuse existing TLS/transport tests unless that implementation changes.
Negative proof
covers Authorization conflict, secret files, safe diagnostics, token redirect,
limit/timeout, and absence of resource dispatch. Test constructors remain
`cfg(test)` or the existing dev-only `test-support`; no production HTTP bypass.

Initializer wiring adds `outbound_auth` to argument, environment, state, inventory,
sync/migration and profile-owner paths, preserving old locks with default `none`.
Effective HTTP prerequisite selection is saved, not inferred differently during
sync. Extend dependency reachability pruning for the new crate and feature edges.
Profile proof uses representative OAuth-only/no-DB, OAuth+JWT, OAuth+introspection,
and the existing maximal compatible service graph, reusing unchanged no-OAuth
coverage. Inspect retained and removed graphs for OAuth and shared HTTP
reachability. Do not multiply by every harness/database/profile permutation or
repeat identical full builds; harness projections remain separate static proof.
Heavy validation remains CI-owned.

Exact-head CI and whole-result review belong to delivery; no deployment or image
publication is implied by template proof.
<!-- template:end outbound-auth:docs-outbound-machine-authentication-decisions -->

<!-- template:begin outbound-auth-grpc:docs-oauth-grpc-decision -->
The concrete gRPC binding stays inside `Credentials`. It injects one bearer,
does not replay, and evicts only from the initial `UNAUTHENTICATED` status or
HTTP 401 without `grpc-status`. An acquisition failure that may pass unchanged
later (local `AtCapacity`, transport, provider 5xx or 429) is `UNAVAILABLE`; a refusal, an
unusable response, or an unsignable assertion is `UNAUTHENTICATED`, the code
gRPC assigns to credentials that failed to produce call metadata, as
grpc-java separates retryable from other credential failures. One
`UNAVAILABLE` for both invited a retry of a request that needs a different
key or grant. The message stays fixed and the closed `AcquisitionError` is the
status source, never sent to a peer. `PreparedCall` captures the gRPC caller and
transport contexts before acquisition, and remains the only owner of
`grpc-timeout` propagation; OAuth adds the bearer only through its prepared
headers, then sends the prepared call. Its optional dependency points from OAuth to
`infra-grpc`; removing either profile removes the bridge. Generated clients
take the concrete authenticated `Service`. See the [transport decision
record](grpc-decisions.md).
<!-- template:end outbound-auth-grpc:docs-oauth-grpc-decision -->
