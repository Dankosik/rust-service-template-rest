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
Custom duplicate members use last-member semantics; consumed standard duplicates
still reject verification. Access never changes normalized identity or scopes,
and Debug never exposes claims. Authentication does not create role or tenant policy. A handler may use `infra_http::authn::require_scope` for
one explicit scope decision; a missing scope is `403 forbidden` with the
standard insufficient-scope bearer challenge.

## Route and failure contract

The assembled OpenAPI document is the route policy source. With an authentication
profile, its root bearer security is the protected default: an operation that
does not override `security` inherits it. An operation is public only with an
explicit `security: []`; public probes ignore `Authorization` and make no
provider call. `x-security-decision`, when an author supplies it, must agree
with that effective policy in the OpenAPI gate. Ambiguous effective security,
unknown schemes and unsupported scoped or anonymous alternatives fail startup;
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
body. Operator diagnostics use closed reasons and the bounded safe preparation
context described below.

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
string or array; otherwise non-null `scp` wins. The unselected value shape is
ignored, but duplicate consumed members still reject. Scopes retain exact case
and become sorted and unique; string separators are ASCII spaces without empty
internal elements. Malformed selected scope is invalid JWT evidence or
unavailable introspection evidence.

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
```

JWT mode requires issuer and one or more exact audiences. `audience` accepts a
string or a nonempty list; duplicate values normalize harmlessly. Supported
algorithms are `RS256`, `ES256`, `PS256`, and `EdDSA`; `RS256` is the
default. `token_profile` is `resource-server` by default or `rfc9068` when
its additional access-token claims are required.

The verifier discovers only metadata whose issuer exactly equals configuration
and installs usable JWKS before serving. Discovery and JWKS admit bounded
HTTP 200 responses with valid expected JSON regardless of Content-Type. Each usable key has a configured,
compatible algorithm binding; a token header never chooses one. Mixed key sets
may retain usable entries while malformed or incompatible entries are skipped.
Ambiguous eligible keys, an invalid signature, or invalid typed issuer,
audience, expiry, not-before, or identity evidence are invalid tokens. The
resource-server profile requires a subject or coherent client identity; RFC 9068
also requires access-token `typ`, subject, `client_id`, `jti`, and `iat`.

Discovery and initial keys share the six-second startup budget. Refresh runs
every 15 minutes and may coalesce an unknown-key refresh with a 30-second
cooldown. One process-owned fetch has its own three-second cap; each waiting
request may be cancelled by the outer HTTP timer without cancelling that work. A
successful refresh atomically replaces keys, while a failed refresh preserves
the last usable snapshot. Refresh is not immediate revocation and does not add
a readiness probe. Bootstrap cancels and joins the refresh task through the
existing background tracker.

`jsonwebtoken` 11.1.0 performs verified decode and registered-claim validation
with the selected aws-lc backend. Typed claims retain duplicate and malformed
evidence rejection; no second signature verifier or generic claims visitor is
used. Revisit this choice only for a new trust profile or token dialect.
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

Set `cache_enabled = true` to reuse successfully verified active results.
`cache_capacity` defaults to 256 and accepts 1–1024 entries; `cache_ttl` defaults
to `"30s"` and accepts human durations from `"1s"` through `"5m"`. All three keys
belong only to introspection mode, and the adapter options constructor validates bounds during bootstrap even while
caching is disabled.
The environment equivalents are `APP__AUTHN__CACHE_ENABLED`,
`APP__AUTHN__CACHE_CAPACITY`, and `APP__AUTHN__CACHE_TTL`.

A hit requires the exact token and the same prepared verifier's immutable trust
context. Its lifetime is fixed at verification completion and ends at the
earlier of the configured TTL and token `exp`, without expiry leeway; hits
never extend it. Current token temporal validity is rechecked on each hit. A valid hit avoids the provider exchange and its capacity permit.
Enabled caching deliberately delays detection of revocation and provider
outages until the cached result expires. Disable it when every request must
observe the provider, and recreate the verifier to discard retained state.

Inactive or invalid tokens, malformed responses, provider failures, and timeouts
are never cached. Expired entries use the normal provider path, including during
an outage, with no stale fallback. Moka coalesces concurrent misses for the same
token within one verifier; live hits and coalesced waiters use no extra provider
permit. Keys are SHA-256 digests rather than raw tokens and are never logged.
Admission may evict entries, and best-effort capacity can temporarily exceed the
configured count; there is no strict aggregate memory bound. Each entry's
retained variable data, including custom claims, is at most 64 KiB. Larger valid
results and successes with no remaining retention lifetime are returned without
caching. Cancelling a waiter does not remove a live entry or strand other
waiters; a surviving caller may retry a cancelled population under the same
provider limit. No cache-fill task is spawned; dropping the last verifier
releases its store.

`provider_concurrency` is a nonzero provider limit and defaults to 32. The adapter rejects
excess work immediately rather than queueing it. An inactive token or active
evidence with wrong issuer, audience, expiry, or not-before is an invalid-token
response. Missing required issuer, audience, expiry, or subject/client identity
also produces an invalid-token response. Wrongly typed supplied claims and
malformed provider evidence are unavailable trust. Omitted `nbf` is allowed,
while present `nbf: null` is
unusable provider evidence.

Credential components use standard form encoding before Basic authentication;
the Authorization header is marked sensitive. The request
body contains only `token` and `token_type_hint=access_token`; a response must
be a bounded HTTP 200 JSON object with the existing JSON media-type policy. `active=false` ignores remaining claim
meaning. The existing `url` and Base64 libraries supply the required encoding;
an OAuth client library would not own these response and identity rules.
<!-- template:end oidc-introspection:authentication-introspection -->

## Provider boundary and operations

`authn_verifications_total` records one HTTP authentication outcome per
protected request, including envelope rejection. `authn_token_verifications_total`
records decisions that reach a verifier engine, with closed `mode`, `outcome`,
and `reason` labels. Keep these counts separate when querying outcomes.
Preparation errors identify closed phase/reason values and static field labels.
Configuration and provider Debug views redact trust inputs, including endpoint
queries and audiences. Tokens, credentials, raw key material, response
bodies and unfiltered provider errors are never diagnostic fields.

Provider calls use only operator-configured or issuer-validated discovery HTTPS
destinations. Normal certificate and hostname verification stay enabled; private
HTTPS IdPs are supported. Caller input never selects a destination. Redirects,
ambient proxies, and retries are disabled. Responses have a 1 MiB ceiling, each
provider attempt has an independent three-second cap through body completion.
Authentication accepts no request deadline and has no response reserve. Dropping
a request cancels its introspection exchange; process-owned JWKS refresh remains
independent and is cancelled and joined at shutdown.

The pooled `reqwest` client owns ordinary runtime connection resources. It adds
no readiness probe or periodic connection check. Authentication deliberately
does not use the outbound HTTP profile's post-resolution address admission or
its DNS owner: that policy remains necessary for independent untrusted outbound
destinations. Generated CA and named-leaf test fixtures with current validity prove ordinary
TLS and name validation without a production provider.

The provider client limits header count but does not promise a separate
application-selected aggregate outbound-header-byte cap. The service's inbound
header limit and native `431` behavior remain separate. The default-off
`test-support` feature mounts the real verifier for consumer tests; it exposes
no verification bypass or principal constructor. [Initializer validation](template-sync.md#validation-boundary)
separates profile projection proof from runtime build and test evidence.
