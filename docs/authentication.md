# Optional inbound authentication

The initializer retains this guide only with an authentication profile. `AUTHN=none`
removes the complete authentication surface. Retaining `oidc-jwt` or
`oidc-introspection` makes that engine available; runtime `authn.mode = "none"`
keeps a public-only service provider-free. A document with an effectively protected
operation cannot start in that mode.

Authentication verifies caller identity and exposes only the sealed principal's
issuer, subject, client ID, `scopes()` and `expires_at()`. It does not create a
role or tenant policy. A handler may use `infra_http::authn::require_scope` for
one explicit scope decision; a missing scope is `403 forbidden` with the
standard insufficient-scope bearer challenge.

## Route and failure contract

The assembled OpenAPI document is the route policy source. With an authentication
profile, its root bearer security is the protected default: an operation that
does not override `security` inherits it. An operation is public only with an
explicit `security: []`; public probes ignore `Authorization` and make no
provider call. `x-security-decision`, when an author supplies it, must agree
with that effective policy. Unsupported or contradictory policy fails startup.

The final HTTP layer enforces the resulting policy before idempotency admission
and handler extraction, preserving native `404`, `405`, and implicit-HEAD
behavior. A registered handler missing from the document is an `internal_error`
instead of an undocumented route. After successful authentication the raw
`Authorization` field is removed, so only verified identity crosses the
boundary.

The boundary accepts exactly one `Authorization` field with a case-insensitive
`Bearer` scheme and RFC 6750 token alphabet. Missing credentials or one
well-formed unsupported scheme produce `401 authentication_required`; duplicate
headers or malformed bearer syntax produce `400 authentication_malformed`; a
token over the effective bound produces `431 authentication_oversize` when the
middleware sees it. Invalid token evidence is `401 authentication_invalid`.
Unavailable trust or provider capacity is `503 authentication_unavailable`, and
a request that exhausts its own remaining budget is `504 request_timeout`.
Caller Problems never include a token, identity, endpoint, key ID, or provider
body. Operator diagnostics use closed reasons and the bounded safe preparation
context described below.

## Configuration

Authentication configuration is mode-specific and rejects unknown or foreign
fields. `none` is the omitted default and accepts no dormant provider inputs.
Issuer, audience, and identities are exact strings: do not trim, normalize, or
case-fold them. Provider URLs are absolute HTTPS URLs with no userinfo, query,
fragment, whitespace, or controls; the adapter owns that one URL grammar before
it performs I/O.

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
and installs usable JWKS before serving. Each usable key has a configured,
compatible algorithm binding; a token header never chooses one. Mixed key sets
may retain usable entries while malformed or incompatible entries are skipped.
Ambiguous eligible keys, an invalid signature, or invalid typed issuer,
audience, expiry, not-before, or identity evidence are invalid tokens. The
resource-server profile requires a subject or coherent client identity; RFC 9068
also requires access-token `typ`, subject, `client_id`, `jti`, and `iat`.

Discovery and initial keys share the six-second startup budget. Refresh runs
every 15 minutes and may coalesce an unknown-key refresh with a 30-second
cooldown. One process-owned fetch has its own three-second cap; each waiting
request keeps its own deadline and may leave without cancelling that work. A
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
belong only to introspection mode, and bounds are validated even while disabled.
The environment equivalents are `APP__AUTHN__CACHE_ENABLED`,
`APP__AUTHN__CACHE_CAPACITY`, and `APP__AUTHN__CACHE_TTL`.

A hit requires the exact token and the same prepared verifier's immutable trust
context. Its lifetime is fixed at verification completion and ends at the
earlier of the configured TTL and token `exp`, without expiry leeway; hits
never extend it. Current token temporal rules and the request deadline still
apply. A valid hit avoids the provider exchange and its capacity permit.
Enabled caching deliberately delays detection of revocation and provider
outages until the cached result expires. Disable it when every request must
observe the provider, and recreate the verifier to discard retained state.

Inactive or invalid tokens, malformed responses, provider failures, and timeouts
are never cached. Expired entries use the normal provider path, including during
an outage, with no stale fallback. Full or contended storage also uses that path.
Each entry's retained variable data is capped at 64 KiB; larger verified results
are returned normally without being cached. Together with capacity this bounds
retained variable payload to 16 MiB by default or 64 MiB at maximum capacity,
plus map, entry, and allocator overhead. The store adds no background task;
dropping the last verifier releases it.

`provider_concurrency` is a nonzero provider limit and defaults to 32. The adapter rejects
excess work immediately rather than queueing it. An inactive token or active
evidence with wrong issuer, audience, expiry, or not-before is an invalid-token
response. Missing required issuer, audience, expiry, or subject/client identity
also produces an invalid-token response. Wrongly typed supplied claims and
malformed provider evidence are unavailable trust. Omitted `nbf` is allowed,
while present `nbf: null` is
unusable provider evidence.

Credential components are form-encoded before Basic authentication. The request
body contains only `token` and `token_type_hint=access_token`; a response must
be a bounded HTTP 200 JSON object. `active=false` ignores remaining claim
meaning. The existing `url` and Base64 libraries supply the required encoding;
an OAuth client library would not own these response and identity rules.
<!-- template:end oidc-introspection:authentication-introspection -->

## Provider boundary and operations

`authn_verifications_total` records one HTTP authentication outcome per
protected request, including envelope rejection. `authn_token_verifications_total`
records decisions that reach a verifier engine, with closed `mode`, `outcome`,
and `reason` labels. Keep these counts separate when querying outcomes.
Preparation errors identify their phase and reason with bounded safe configured
issuer, audience and endpoint context; issuer mismatch includes a sanitized
discovered issuer when safe. Tokens, credentials, raw key material, response
bodies and unfiltered provider errors are never diagnostic fields.

Provider calls use only operator-configured or issuer-validated discovery HTTPS
destinations. Normal certificate and hostname verification stay enabled; private
HTTPS IdPs are supported. Caller input never selects a destination. Redirects,
ambient proxies, and retries are disabled. Responses have a 1 MiB ceiling, each
provider attempt has a three-second cap, and request-driven work cannot exceed
the remaining request deadline less its 100ms response reserve.

The pooled `reqwest` client owns ordinary runtime connection resources. It adds
no readiness probe or periodic connection check. Authentication deliberately
does not use the outbound HTTP profile's post-resolution address admission or
its DNS owner: that policy remains necessary for independent untrusted outbound
destinations. Synthetic DER fixtures continue to prove ordinary TLS and name
validation without a production provider.

The provider client limits header count but does not promise a separate
application-selected aggregate outbound-header-byte cap. The service's inbound
header limit and native `431` behavior remain separate. The default-off
`test-support` feature mounts the real verifier for consumer tests; it exposes
no verification bypass or principal constructor. [Initializer validation](template-sync.md#validation-boundary)
separates profile projection proof from runtime build and test evidence.
