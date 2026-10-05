# Optional inbound authentication

The initializer retains this guide only with an authentication profile. `AUTHN=none`
removes the complete authentication surface. Retaining `oidc-jwt` or
`oidc-introspection` makes that engine available; runtime `authn.mode = "none"`
keeps a public-only service provider-free. A document with an effectively protected
operation cannot start in that mode.

Authentication verifies caller identity and exposes the sealed principal's
issuer, subject, client ID, `scopes()` and `expires_at()`. Its immutable
`claims<T>()` accessor deserializes the same verified JSON payload into an
application type, with a sanitized `ClaimAccessError` on incompatible shape.
Custom duplicate members use last-member semantics; duplicates of a claim the
verifier reads reject verification. Access never changes normalized identity or scopes,
and Debug never exposes claims. Authentication does not create role or tenant policy.

`access_token()` returns the verified bearer token itself, as a
`&SecretString` with a redacted Debug. It exists for one purpose: RFC 8693
token exchange when calling another service on behalf of the caller
(service-to-service authentication). A handler must never forward it as an
outbound `Authorization` header to any other party.

`actor()` returns the outermost `act` object's `sub` and `client_id` when the
verified evidence carries one, for JWT and introspection alike. A present
`act` that is not an object, whose `sub` is missing, empty or not a string,
or whose `client_id` is not a string, is malformed evidence with the existing
failure class of each engine — an invalid JWT or unavailable introspection
evidence. The outermost actor is the current one and may inform access
decisions (RFC 8693 §4.1); nested actors are informational and are not
exposed, and `may_act` is an authorization-server input that is not read.

Required scopes for a protected operation come from its OpenAPI security
requirement, not a handler-called helper: a request whose principal satisfies
none of the operation's requirements gets `403 forbidden` with the standard
insufficient-scope bearer challenge before the handler runs; see [Route and
failure contract](#route-and-failure-contract).

## Route and failure contract

The assembled OpenAPI document is the route policy source. With an authentication
profile, its root bearer security is the protected default: an operation that
does not override `security` inherits it. An operation is public only with an
explicit `security: []`; public probes ignore `Authorization` and make no
provider call. `x-security-decision`, when an author supplies it, must agree
with that effective policy in the OpenAPI gate. A security requirement's
scopes, for example `{bearerAuth: [billing.write]}`, are the route's declared
policy: several requirement objects are alternatives joined by OR, and the
scopes within one are joined by AND; an empty scope list means any
authenticated caller. Ambiguous effective security, an unknown scheme, and a
mixed requirement — `bearerAuth` combined with another scheme, or an
anonymous `{}` alternative beside a protected one — still fail startup;
response completeness and extension consistency are documentation checks.

The final HTTP layer enforces the resulting policy before idempotency admission
and handler extraction, preserving native `404`, `405`, and implicit-HEAD
behavior. HEAD uses an explicit HEAD policy when documented, otherwise GET.
A matched served operation missing policy is a sanitized `500 internal_error`.
Register operations as `OpenApiRouter::routes(utoipa_axum::routes!(handler))`
([HTTP authoring](architecture/http.md#adding-an-operation)); Clippy rejects
raw routes, fallbacks and separately registered HEAD handlers. After successful
authentication the raw `Authorization` field is removed, so only verified
identity crosses the boundary.

The boundary accepts exactly one `Authorization` field with a case-insensitive
`Bearer` scheme and RFC 6750 token alphabet. Missing credentials or one
syntactically valid foreign scheme produce `401 authentication_required`, without
parsing that scheme's credentials. Duplicate headers or malformed bearer syntax
produce `400 authentication_malformed`. The bounded server owns aggregate header
admission and native `431`; authentication has no separate token-size cap. Invalid token evidence is `401 authentication_invalid`.
Unavailable trust or provider capacity is `503 authentication_unavailable`, and
the outer hardened HTTP timer alone emits `504 request_timeout`. A completed
provider timeout is unavailable trust if the HTTP request is still live.
Caller Problems never include a token, identity, endpoint, key ID, or provider
body. Operator diagnostics use the closed reasons described below.

## Configuration

Authentication configuration is mode-specific and rejects unknown or foreign
fields. `none` is the omitted default and accepts no dormant provider inputs.
Issuer, audience, and identities are exact strings: do not trim, normalize, or
case-fold them. Provider URLs are absolute HTTPS URLs with no userinfo, fragment,
whitespace, or controls. Issuers also forbid queries; discovered JWKS and
configured introspection endpoints allow queries and preserve their spelling
on requests. The adapter validates URLs before I/O. Configuration Debug exposes
only mode, never issuer, audience, endpoint or credentials.

For JWT and active introspection evidence, `scope` and `scp` each accept a
space-delimited string or string array. Non-null `scope` wins, including an empty
string or array; otherwise non-null `scp` wins. Scopes retain exact case and
become sorted and unique; string separators are ASCII spaces without empty
internal elements. A wrongly typed `scope` or `scp`, or a malformed selected
scope, is invalid JWT evidence or unavailable introspection evidence.

JSON `null` means absent for every claim the verifier reads. The client identity
is the first non-empty `client_id`, `azp`, `appid` or `cid`; the subject is a
non-empty `sub`. JWT registered claims (`iss`, `aud`, `exp`, `nbf`) follow
`jsonwebtoken`: a null `exp` is missing and a null `nbf` is malformed.

The repository [engineering policy](../AGENTS.md#engineering) governs normative
protocol requirements and any stricter application rule.

<!-- template:begin oidc-jwt:authentication-jwt -->
## OIDC JWT

Use this nonsecret configuration, substituting the issuer and intended audience:

```toml
[authn]
mode = "oidc-jwt"
issuer = "https://issuer.example"
audience = "catalog-api"
# algorithms = ["RS256"]
# token_profile = "rfc9068" # omitted means "resource-server"
# jwks_uri = "https://issuer.example/keys" # omitted means discovery
```

JWT mode requires issuer and one or more exact audiences. `audience` accepts a
string or a nonempty list; duplicate values normalize harmlessly. Supported
algorithms are `RS256`, `ES256`, `PS256`, and `EdDSA`; `RS256` is the
default. `token_profile` is `resource-server` by default or `rfc9068` when
its additional access-token claims are required.

The verifier discovers only metadata whose issuer exactly equals configuration
and installs usable JWKS before serving. Discovery requests the OIDC document
at `{issuer}/.well-known/openid-configuration`; when the provider answers that
location with a status other than 200, it requests RFC 8414 metadata at
`/.well-known/oauth-authorization-server` followed by the issuer path. Set
`jwks_uri` (`APP__AUTHN__JWKS_URI`) for a provider that publishes neither: the
verifier then skips discovery and loads keys from that HTTPS endpoint, and a
token's `iss` is still compared with `issuer` exactly. Discovery and JWKS admit bounded
HTTP 200 responses with valid expected JSON regardless of Content-Type. A key
with its own `alg` serves only that configured algorithm; a key without `alg`
serves every configured algorithm of its type (RSA: `RS256` and `PS256`), as
Nimbus and go-oidc do. The token header `alg` must be configured and fit the
key. Mixed key sets retain usable entries while malformed or incompatible
entries are skipped. Every installed key that matches the token's `kid` (or
every key, when the token has none) and algorithm is tried in turn; the first
valid signature wins. A token whose `typ` header names another kind of JWT is
invalid: the resource-server profile admits only an absent `typ`, `JWT`
(or `application/jwt`) and `at+jwt` (or `application/at+jwt`), so a logout
token, a client assertion or a DPoP proof signed by the same issuer is never
an access token (RFC 8725 section 3.11). An untyped ID token stays
indistinguishable in this profile except by its missing scopes; `rfc9068`
requires `at+jwt`. An invalid signature or invalid issuer, audience, expiry,
not-before or identity evidence is an invalid token. The resource-server
profile requires a subject or client identity; RFC 9068 also requires
access-token `typ`, subject, `client_id`, `jti`, and `iat`.

Discovery and initial keys share the six-second startup budget and are not
retried. JWT mode therefore needs the provider at startup: a replica started
during a provider outage exits with the preparation error and relies on the
platform's restart policy, while running replicas keep verifying with their
installed keys. The error names the provider failure class, for example
`Discovery: Fetch(Status(404))`, and a failed refresh logs the same class as
`cause`. Refresh runs
every 15 minutes. A token whose `kid` names no installed key, or a kid-less
token no installed key verifies, requests a refresh with a 30-second cooldown;
during the cooldown the token is invalid after a successful fetch and
unavailable after a failed one. A token that missed while a fetch was
installing new keys is checked once against those keys instead of being
refused by the cooldown. One process-owned fetch has its own three-second cap; each waiting
request may be cancelled by the outer HTTP timer without cancelling that work. A
successful refresh atomically replaces keys, while a failed refresh preserves
the last usable snapshot. Refresh is not immediate revocation and does not add
a readiness probe. Bootstrap cancels and joins the refresh task with its
other background tasks, and stops the service if the task ends on its own.

The installed key set has no hard expiry: repeated refresh failures can
leave a known key trusted indefinitely while token lifetime checks continue.
Application replicas keep independent snapshots. The service and its issuer
must accept the resulting key-removal lag and define emergency trust removal
before using this mode for a contract that requires prompt revocation.
The refresh interval is not a maximum trust age.

Each admitted key is parsed once into an aws-lc `ParsedPublicKey` per
algorithm it serves, and a token's signature is checked against those keys
directly. `jsonwebtoken` 11.1.0 still supplies the JWK, header and algorithm
types, and the crate reads registered claims with its rules: `iss` and `aud`
are a string or an array that must name a configured value, `exp` is required,
a numeric `exp`, `nbf` or `iat` may be fractional (RFC 7519 `NumericDate`) and
is rounded, and lifetime is checked before issuer and audience. A test signs a
corpus of claim shapes, payloads and compact forms and compares each decision
with `jsonwebtoken::decode`; a header the library refuses never passes here.
Two `proptest` properties extend that comparison to generated combinations of
registered-claim shapes, in plain and `\u`-escaped spelling, and to payloads
damaged at random positions. They found one difference, kept on purpose: a
payload that is not valid UTF-8 is refused here (RFC 7519 section 7.2), while
the library accepts one whose invalid bytes sit in a member it skips. The payload is decoded and read once. `jsonwebtoken::decode` was
replaced because it rebuilds the aws-lc key on every call and parses the header
three times and the payload three times; the direct path cut full RS256
verification by about 40% (see
[bearer authentication performance](bearer-authentication-performance.md)).
Revisit when `jsonwebtoken` verifies with pre-parsed keys, or for a new trust
profile or token dialect.

Discovery and JWKS refresh stay in this crate: `jwt-authorizer` 0.15 (last
release 2024-08) targets `jsonwebtoken` 9, `reqwest` 0.12 and `axum` 0.7, and
`tower-oauth2-resource-server` lacks unknown-key refresh and per-key skipping.
Revisit when a maintained crate covers discovery, unknown-key refresh with a
cooldown, and `jsonwebtoken` 11.
<!-- template:end oidc-jwt:authentication-jwt -->

<!-- template:begin oidc-introspection:authentication-introspection -->
## OIDC introspection

Use this nonsecret configuration, substituting the provider values:

```toml
[authn]
mode = "oidc-introspection"
issuer = "https://issuer.example"
audience = "catalog-api"
introspection_endpoint = "https://issuer.example/introspect"
introspection_client_id = "catalog-api"
# provider_concurrency = 32
# cache_enabled = false
# cache_capacity = 256
# cache_ttl = "30s"
```

Set the credential only through
`APP__AUTHN__INTROSPECTION_CLIENT_SECRET`; never put it in TOML. With the default
`cache_enabled = false`, each admitted opaque token makes one RFC 7662 POST
with `token_type_hint=access_token` and client-secret Basic authentication.
There is no retry, redirect, or remembered outage.
Same-token misses are not coalesced in this disabled-cache mode. Do not
enable positive retention solely to reduce provider load when every request
must observe the provider.

Set `cache_enabled = true` to reuse successfully verified active results.
`cache_capacity` defaults to 256 and accepts 1–1024 entries; `cache_ttl` defaults
to `"30s"` and accepts human durations from `"1s"` through `"5m"`. All three keys
belong only to introspection mode, and the adapter options constructor validates bounds during bootstrap even while
caching is disabled.
The environment equivalents are `APP__AUTHN__CACHE_ENABLED`,
`APP__AUTHN__CACHE_CAPACITY`, and `APP__AUTHN__CACHE_TTL`.

A hit requires the exact token and the same prepared verifier's immutable trust
context. Its lifetime is fixed when the result is stored and ends at the
earlier of the configured TTL and token `exp`, without expiry leeway; hits
never extend it. Moka enforces that lifetime on its own monotonic clock, so a
wall-clock step after storage does not shorten or extend it. A valid hit avoids
the provider exchange and its capacity permit.
Enabled caching deliberately delays detection of revocation and provider
outages until the cached result expires. Disable it when every request must
observe the provider, and recreate the verifier to discard retained state.

Inactive or invalid tokens, malformed responses, provider failures, and timeouts
are never cached. Expired entries use the normal provider path, including during
an outage, with no stale fallback. Moka coalesces concurrent misses for the same
token within one verifier; live hits and coalesced waiters use no extra provider
permit. Keys are SHA-256 digests rather than raw tokens and are never logged.
Each retained result also holds the presented token as a `SecretString`,
because the principal exposes it as the subject of an RFC 8693 token exchange.
Admission may evict entries, and best-effort capacity can temporarily exceed the
configured count; there is no strict aggregate memory bound. Each entry's
verified provider payload, including custom claims, is at most 64 KiB. That
ceiling excludes the presented access token, normalized fields and allocation
overhead; cache capacity also excludes pending callers and provider responses.
The inbound server bounds request headers, not this library's direct callers.
Larger valid results and successes with no remaining retention lifetime are
returned to every waiter
without reusable retention. Cancelling a waiter preserves the original fill;
cancelling the initializer lets a surviving caller start its own provider
exchange under the same provider limit. A completed error is shared with the
current waiters but is not retained for later calls. No cache-fill task is
spawned; dropping the last verifier releases its store.

Application replicas cache independently, so revocation visibility can differ
within the accepted TTL. The local provider concurrency limit does not bound
the fleet's attempts or the rate of fast failures. Keep expired results on
the provider path during an outage; stale-while-revalidate would change the
authorization contract.

`provider_concurrency` is a nonzero provider limit and defaults to 32. The adapter rejects
excess work immediately rather than queueing it. An inactive token or active
evidence with wrong issuer, audience, expiry, or not-before is an invalid-token
response. Missing required issuer, audience, expiry, or subject/client identity
also produces an invalid-token response; a null claim counts as missing.
Wrongly typed supplied claims, duplicate read claims and malformed provider
evidence are unavailable trust. A failed provider exchange is unavailable trust
counted under one closed `reason`: `provider_timeout`, `provider_connect`
(DNS, TCP or TLS), `provider_status_4xx` (for example rejected client
credentials), `provider_status_5xx`, `provider_status_other`,
`provider_media_type`, `provider_too_large` or `provider_transfer`.

Credential components use standard form encoding (RFC 6749 section 2.3.1)
before `reqwest`'s Basic authentication, which marks the header sensitive. The
request body contains only `token` and `token_type_hint=access_token`; a
response must be a bounded HTTP 200 JSON object with the existing JSON
media-type policy. `active=false` ignores the rest of the body. The existing `url` crate and `reqwest` Basic authentication supply the required encoding;
an OAuth client library would not own these response and identity rules.
<!-- template:end oidc-introspection:authentication-introspection -->

## Provider boundary and operations

`authn_verifications_total` records one authentication outcome per protected
request, including envelope rejection and cancellation, with a `transport`
label of `http` or `grpc`. Both transports use `Verifier::authenticate`. `authn_token_verifications_total`
records decisions that reach a verifier engine, with closed `mode`, `outcome`,
and `reason` labels. Keep these counts separate when querying outcomes.
Preparation errors identify closed phase/reason values and static field labels;
an issuer mismatch also names the configured and the discovered issuer, echoing
the discovered value only when it is an issuer URL of at most 256 bytes. A
failed provider fetch names one closed class: `Timeout`, `Connect` (DNS, TCP or
TLS), `Status(code)`, `MediaType`, `TooLarge` or `Transfer`.
`authn_provider_request_duration_seconds` records how long each provider
exchange took, with an `operation` label of `discovery`, `jwks` or
`introspection` and an `outcome` label of `success`, `cancelled` or one closed
failure class: `provider_timeout`, `provider_connect`, `provider_status_4xx`,
`provider_status_5xx`, `provider_status_other`, `provider_media_type`,
`provider_too_large` or `provider_transfer`. It uses the default histogram
buckets. Each exchange also runs in an `authn_provider` client span, exported
under its method name, that carries the operation, the provider host and port,
the status of a successful or status-refused response and, when the exchange
failed, the same class as `error.type`; never a path, query or credential. An introspection span is a child of the
request's span; a key refresh has no request and starts its own trace. A
caller that stops waiting, including the startup budget, is `cancelled`.
A system clock before the Unix epoch reads as the far future, so a token is
refused as expired rather than admitted.
Configuration and provider Debug views redact trust inputs, including endpoint
queries and audiences. Tokens, credentials, raw key material, response
bodies and unfiltered provider errors are never diagnostic fields.

Provider calls use only operator-configured or issuer-validated discovery HTTPS
destinations. Normal certificate and hostname verification stay enabled; private
HTTPS IdPs are supported. Caller input never selects a destination. Redirects,
ambient proxies, and retries are disabled. Responses have a 1 MiB ceiling, and
each provider attempt has `reqwest`'s three-second total timeout, which covers
body completion.
Authentication accepts no request deadline and has no response reserve. Dropping
a request cancels its introspection exchange; process-owned JWKS refresh remains
independent and is cancelled and joined at shutdown.

The pooled `reqwest` client owns ordinary runtime connection resources. It adds
no readiness probe or periodic connection check. Authentication has its own
trusted-provider transport and does not share the outbound HTTP client.
Shared generated test material continues to prove ordinary TLS and name
validation without a production provider.

The provider client limits header count. The service's inbound header limit and
native `431` behavior remain separate. The default-off
`test-support` feature mounts the real verifier for consumer tests; it exposes
no verification bypass or principal constructor. [Initializer validation](template-sync.md#validation-boundary)
separates profile projection proof from runtime build and test evidence.
