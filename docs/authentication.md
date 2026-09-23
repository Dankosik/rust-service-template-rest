# Optional inbound authentication

The initializer retains this guide only with an authentication profile. `AUTHN=none`
removes it and every authentication-specific runtime surface. Retaining an
engine does not enable it: runtime `authn.mode` still defaults to `none`, needs
no credentials, makes no provider request, and leaves public routes public.

Authentication verifies a caller identity only. It does not grant scopes, roles, tenant membership, or other business authorization. A protected operation is composed with `infra_http::authn::protect` and receives a sealed `VerifiedPrincipal`, whose issuer, optional subject, and optional client ID were verified. Handlers cannot create that type, read the raw Authorization value, or consume unverified provider data.

## Route and failure contract

Only operations with the real `bearerAuth` security requirement and `x-security-decision=protected` use the protection helper. Public operations, including `/health/live` and `/health/ready`, explicitly use `security: []`; they do not authenticate arbitrary Authorization headers or contact a provider. The retained profile declares the actual bearer scheme and global default for protected operations. This is the documented exception to the earlier no-unused-scheme convention; public operations must keep their explicit override.

The boundary accepts exactly one `Authorization` field with a case-insensitive `Bearer` scheme and RFC 6750 token alphabet. Missing credentials produce 401 `authentication_required`; malformed syntax produces 400 `authentication_malformed`; a token exceeding 32 KiB produces 431 `authentication_oversize`. Invalid verification produces 401 `authentication_invalid`; disabled or unavailable trust and exhausted provider capacity produce 503 `authentication_unavailable`. The enclosing request timeout remains the existing 504 `request_timeout` owner. Problems and observability never include a token, identity, endpoint, key identifier, or provider body.

Declare the protected decision/rationale, exact bearer requirement and
400/401/403/431/503/504 response family on the same `routes!` tuple passed to
`protect`. Authentication uses the registered method's route layer, preserving
404/405 fallbacks. The extractor reads only the sealed principal; missing
principal or request deadline is a sanitized wiring fault. Authorization headers
are removed before the protected handler. A token failure never grants a role or
returns an authorization decision; 403 belongs to downstream policy.

## Configuration

All values are strict and unknown fields fail startup. Provider URLs are absolute
HTTPS with no userinfo, query, or fragment. Issuer, audience, and identities
are exact strings: do not trim, normalize, or case-fold them.

<!-- template:begin oidc-jwt:authentication-jwt -->
## OIDC JWT

Use this nonsecret configuration, substituting your issuer and intended audience:

```toml
[authn]
mode = "oidc-jwt"
issuer = "https://issuer.example"
audience = "catalog-api"
# token_profile = "rfc9068" # omitted means "resource-server"
```

JWT mode requires issuer and audience; its optional `authn.token_profile` is
`resource-server` by default or `rfc9068` for strict RFC 9068 access-token
rules. The verifier discovers exact issuer metadata, installs usable JWKS before
serving, and accepts only signed RS256 access tokens with eligible 2048–8192-bit
RSA keys. Both profiles require exact issuer/audience, expiry, and a subject or
client identity. `rfc9068` also requires access-token `typ`, subject,
`client_id`, `jti`, and valid `iat`.

Discovery and initial JWKS share a six-second startup budget. JWKS refresh runs
every 15 minutes and can coalesce an unknown-key refresh, with a 30-second
cooldown and one active fetch. Each fetch has a three-second cap; a request
reservation transfers its absolute deadline to the worker, including scheduling
delay. Successful refresh replaces the key set, including removals. Failed
refresh retains the last usable set. There is no maximum key-cache age or
immediate revocation guarantee, and refresh failures do not change readiness.
The process-owned refresh future is cancelled and joined through the existing
background tracker and budget.

Consumed header, claim and RSA key fields reject duplicates, wrong types and
explicit nulls. Key metadata must permit signature verification without signing
or encryption/key-management operations. With `kid`, exactly one eligible key
must match; without it, the set must contain exactly one eligible key. Both token
profiles use integral nonnegative epoch seconds with 30 seconds of leeway and
overflow-safe arithmetic. Resource-server client aliases (`client_id`, `azp`,
`appid`, `cid`) must agree when several occur; RFC 9068 requires its own
`client_id`, subject, `iat`, `jti` and access-token type.

The selected crypto mechanism is `jsonwebtoken` 11.1.0 with `aws_lc_rs`, no PEM
or alternate crypto feature. Template code retains strict consumed-JSON policy
and refresh ownership. A generic authorizer with an incompatible stack and a
full interactive OIDC client did not fit this access-token boundary. Revisit
that choice for another algorithm or token dialect; normal dependency upgrades
still follow the repository's dependency policy.
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
```

Set the credential only through
`APP__AUTHN__INTROSPECTION_CLIENT_SECRET`; never put it in TOML. Each admitted
syntactically valid token makes one RFC 7662 POST with
`token_type_hint=access_token` and client-secret Basic authentication. The token
stays opaque. There is no cache, retry, redirect, or remembered failure; an
outage affects that request only.

The client admits at most 32 exchanges without a queue. A valid inactive or trust/temporal mismatch is an invalid-token response; malformed or unusable provider evidence is unavailable trust. It does not change process readiness.

Form-encode each client credential component before constructing Basic
authentication. The request body contains only `token` and
`token_type_hint=access_token`. A response must be a bounded HTTP 200 JSON object;
duplicate top-level names and trailing JSON refuse. `active=false` ignores
remaining claim meaning. Active responses require complete typed issuer,
audience, expiry and subject or client identity before mismatch classification.
The existing `url` and Base64 libraries supply encoding; adding an OAuth client
library would not replace the strict response, bounded transport or identity
policy, so none is introduced for this two-field POST.
<!-- template:end oidc-introspection:authentication-introspection -->

## Provider boundary and operations

Provider calls use only exact configured or discovery-validated HTTPS destinations, normal certificate and hostname validation, and public-unicast resolution on each connection. Loopback, private, link-local, multicast, unspecified, and other reserved destinations are refused. Mapped IPv4 addresses undergo the same public-unicast predicate. Redirects, ambient proxies, retries, caller-selected destinations, and connection reuse are not supported. Responses are read with a 1 MiB ceiling. Provider work has a three-second absolute cap and, for requests, cannot exceed the remaining request deadline less a 100ms response reserve.

The provider client limits header count, but does not promise a separate application-selected aggregate outbound header-byte cap. The service's inbound header limit and native 431 behavior remain separate. The default-off `test-support` feature is for tests that mount the real verifier with fixture transport; it is not enabled in ordinary builds and exposes no verification bypass or principal constructor.

`reqwest` 0.13 supplies the fixed private HTTPS client with system roots. HTTP/1
and zero idle pooling avoid reused-connection replay beyond its retry policy;
pooling or HTTP/2 needs a fresh no-replay assessment. Hickory 0.26.3 supplies
asynchronous system-DNS resolution. Each lookup owns a cancellation scope and
tracker token, uses the actual admitted answer set, and retains the original
hostname for certificate verification. System DNS configuration is snapshotted
at preparation and changes require restart; no fallback public resolver is
invented. DNS wrapper futures join the existing background budget. Reqwest's
internal HTTP dispatchers retain cancellation-on-drop and
runtime-teardown ownership; a tracker receipt does not claim their direct join.

These are authentication-specific controls, not a general outbound HTTP
capability. Another provider class, private-network exception, revocation rule,
authorization policy or transport lifetime mechanism reopens the corresponding
contract. Synthetic DER fixtures exercise normal TLS/name checks; renew expired
fixtures without weakening verification. [Initializer validation](template-sync.md#validation-boundary)
separates all-choice projection proof from distinct runtime build/test evidence.
