# Service-to-service authentication

<!-- template:begin outbound-auth:docs-service-to-service-authentication -->
## Mandate

The only supported way one service calls another: a short-lived,
audience-bound JWT access token from the shared authorization server. A
caller obtains its own service token by OAuth 2.0 client credentials with
`private_key_jwt` client authentication; it carries a verified user's context
by RFC 8693 token exchange; a receiver verifies the token with
`AUTHN=oidc-jwt` and enforces the scopes declared for the operation called:
in OpenAPI for HTTP, at service registration for gRPC.

The mandate covers synchronous HTTP and gRPC calls. Durable messaging is
outside it: the broker connection authenticates with NATS credentials, and an
event carries no verified user identity.

Explicitly forbidden, with each reason:

- **Static bearer tokens or API keys between services.** They carry no
  expiry, no receiver-bound audience, and widen exposure every time they are
  copied.
- **HS256 or any shared JWT secret.** Every holder of the secret can mint
  tokens every other holder accepts, so one compromised service forges any
  identity. Client secrets at the authorization server are
  replaced by keys for the same reason (RFC 9700 §2.5).
- **JWTs minted by the calling service.** A caller that signs its own
  identity bypasses the authorization server as the one source of identity
  and scopes.
- **Custom HMAC request signatures.** They replace a maintained,
  standards-reviewed token stack with hand-rolled cryptography and
  homegrown replay bookkeeping.
- **Unsigned identity headers** (for example an `X-…-Principal` user
  header). Anything on the same network hop can set or overwrite an
  unsigned header; proxy-injected identity must be signed and verified, not
  trusted (RFC 9700 §4.13).
- **Forwarding a received access token to another service.** A token is
  minted for one audience; replaying it to a different receiver violates
  audience restriction (RFC 9700 §2.3) and defeats per-integration scope and
  revocation boundaries.

## Flow overview

1. **Caller, no user context:** acquire an audience-bound access token by
   client credentials → call the receiver with that token.
2. **Caller, on behalf of a user:** exchange the verified inbound access
   token for one addressed to the next integration → call the receiver with
   the exchanged token.
3. **Receiver:** verify the bearer token → enforce the operation's declared
   scopes → run the handler.

## Caller setup

Select the profile with `OUTBOUND_AUTH=oauth2-client-credentials`. Configure
each integration's OAuth tuple in TOML, for example:

```toml
[integrations.billing.oauth]
token_url = "https://identity.example/oauth2/token"
client_id = "billing-service"
key_id = "billing-service-2026"
algorithm = "RS256"
assertion_audience = "https://identity.example/"
scopes = ["billing.read"]
audience = "billing-api"
```

Supply the private key only through
`APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY`; this is the only place the
PEM may live, and a file value is refused by the secret guard.

Every token request signs a fresh client assertion instead of sending a
secret. Its header is `{alg, kid, typ: "client-authentication+jwt"}`; its
claims are `iss` = `sub` = the client ID, `aud` = `assertion_audience` as one
string, `iat` = `nbf` = now, `exp` = now + 60 seconds, and `jti` = a new
random UUID v4 that is never reused across requests.

`assertion_audience` is the authorization server's issuer identifier, not its
token endpoint. This corrects an earlier common practice: a malicious server
could advertise another server's token endpoint as its own, obtain an
assertion addressed to it, and replay that assertion to impersonate the
client (OIDF disclosure 2025-01-24, CVE-2025-27370/27371;
draft-ietf-oauth-rfc7523bis-11 §4). Use the token
endpoint URL instead only for a server that accepts no other value (Hydra
v26, Okta, Entra); a service configured with one authorization server per
integration is not exposed by that exception.

Rotate a key without downtime: register the new public key at the
authorization server beside the old one, deploy the new `private_key` and
`key_id` together (restart to apply, since configuration is immutable), then
delete the old public key. The service serves no `jwks_uri`; rotation is a
deploy, not a runtime call.

Compose the authenticated client with `credentials.http(client)`. To call on
behalf of a verified user, insert
`OnBehalfOf::new(principal.access_token().clone())` into the outbound
request's extensions before dispatch, using `http::Request::extensions_mut`;
the gRPC binding takes the same value through
`tonic::Request::extensions_mut`. See [Outbound machine
authentication](outbound-machine-authentication.md#acquisition-and-reuse) for
reuse, deadlines, and eviction bounds.

## Receiver setup

Verify inbound calls with the JWT engine:

```toml
[authn]
mode = "oidc-jwt"
issuer = "https://identity.example/"
audience = "billing-api"
# algorithms = ["RS256"]
# token_profile = "resource-server" # "rfc9068" only if the AS emits at+jwt
```

`audience` is this service's own identifier as the authorization server
addresses it; `algorithms` must match what the authorization server signs
with; `token_profile` stays `resource-server` unless the authorization
server emits `at+jwt` access tokens.

Declare each operation's required scopes in its OpenAPI security
requirement, for example:

```rust,ignore
#[utoipa::path(
    ...,
    security(("bearerAuth" = ["billing.write"]))
)]
```

Several requirement objects are alternatives joined by OR; the scopes inside
one object are joined by AND. A verified principal that satisfies no
requirement gets `403 forbidden` with `WWW-Authenticate: Bearer
error="insufficient_scope"`, before idempotency admission and the handler.

A gRPC receiver declares the same policy per method when it registers the
service, with every listed scope required; a principal that lacks one gets
`PERMISSION_DENIED` before the handler:

```rust,ignore
services.add(BillingServiceServer::new(billing))?;
services.require_scopes("/billing.v1.BillingService/Charge", &["billing.write"])?;
```

On either transport an operation with no declared scope admits any caller
holding a valid token for this audience. Declare at least one scope on every
service-to-service operation: a scope is also what separates an access token
from another JWT the same issuer signed for this audience. The verifier
refuses a token whose `typ` header names another kind of JWT (for example
`logout+jwt`), but an untyped ID token is indistinguishable from an untyped
access token except by its missing scopes. Select `token_profile = "rfc9068"`
wherever the authorization server emits `at+jwt` with the RFC 9068 claims; it
closes that gap completely.

`VerifiedPrincipal` exposes `client_id()` (the calling service),
`subject()`, `actor()`, and `scopes()`. Receiver rules:

- Authorize the calling service by `client_id()` and its scopes.
- Authorize access to user-owned data by ownership of `subject()`.
- Never compare `subject()` with a client identifier; the two namespaces are
  not interchangeable across authorization servers.
- `actor()` is the current actor and may inform access decisions alongside
  the top-level claims; nested actors are informational only and are not
  exposed (RFC 8693 §4.1).
- `may_act` is an authorization-server input and is ignored by the receiver.

An already exchanged token stays usable until its own expiry even if the
user's session is revoked in the meantime, the same as any bearer token.
Keep exchanged-token lifetimes short at the authorization server to bound
this lag.

## Choosing an authorization server

The template does not select or host an authorization server; that choice
and its hosting are a service-local decision. This shortlist and its
evidence come from research recorded 2026-09-29:

| Server | Notes |
| --- | --- |
| Keycloak 26.7 | JVM container plus PostgreSQL, Apache-2.0. The only self-hostable candidate covering client assertions, client credentials, GA standard token exchange V2, and per-service audience through client scopes with one client per service. No `act` claim in supported features (the caller shows as `azp`): token exchange delegation, which emits `act`, is experimental in 26.7 and preview in 26.8 (checked 2026-10-01); a service token's `sub` is the service-account user ID; no `audience` parameter on client credentials; roughly 0.75–1.5 GB memory. |
| Spring Authorization Server 7 | Covers the protocol, but it is a Java framework the team would build and operate; `act`, audience mapping, and assertion replay protection are custom code. |
| Zitadel ≥ 4.19.2 | One Go container plus PostgreSQL, AGPL-3.0, GA token exchange with nested `act`. Does not fit this path without a gap (see below). Pin ≥ 4.19.2 for GHSA-vrh8-c9cm-wh8v and GHSA-w4gv-rcwj-w6r5. |

Not shortlisted, with the deciding gap: authentik 2026.8 (token exchange
accepts only a client secret; assertion verification skips `aud` and `jti`),
Hydra (no RFC 8693 support; issue open since 2018), Authelia (no token
exchange yet), Duende (licence cost), Auth0 (`private_key_jwt` is an
Enterprise feature), Okta (per-service audiences need a paid add-on), Entra
and Cognito (no exchange that emits `act`).

Keycloak registration for this path: a confidential client with client
authenticator "Signed JWT" configured with the service's public key or JWKS
URL, service accounts enabled, and standard token exchange enabled on any
client that exchanges tokens. Give the client an audience through client
scopes; the receiving service is itself a client whose ID is that audience.
Keycloak issues JWT access tokens by default, matching `token_profile =
"resource-server"`.

What Zitadel and authentik lack for this path: Zitadel's service users
authenticate only through the RFC 7523 §2.1 JWT-bearer grant, and only a
second registration, an OIDC application with its own key, can call token
exchange — there is no single client identity covering both client
credentials and exchange. authentik's token exchange authenticates only with
a client secret, so it cannot use `private_key_jwt` for the exchange leg at
all.

## Migration from the current schemes

Move a derived service off its homegrown schemes with a temporary
dual-acceptance window instead of a flag day:

1. The receiver accepts both the legacy scheme and this guide's token at the
   same time, choosing which one verified a request by an unambiguous
   signal — for example a distinct header or a distinct route — never by
   falling back to the legacy scheme after this scheme's verification fails.
2. The receiver counts admitted requests per scheme.
3. Callers move to this guide's path one at a time.
4. Once a scheme's count stays at zero, the receiver removes that scheme's
   acceptor code and deletes its secrets.

The legacy acceptor is service-local code owned by the derived service, not
template code; the template never carries a second, permanent way to
authenticate a service-to-service call.

| Current scheme | Replacement |
| --- | --- |
| Static bearer token | Audience-bound access token by client credentials (this guide's mandate). |
| HS256 shared secret | `private_key_jwt` client authentication (no shared secret). |
| Caller-signed RS256 JWT with hand-copied keys | Authorization-server-issued token, verified by `oidc-jwt` discovery — no manually distributed keys. |
| Payments' HMAC request signature and replay store | Only if request integrity beyond TLS is still required: RFC 9421 HTTP Message Signatures, not a custom HMAC. |
| Payments' replay store / billing's single-use `jti` used for business deduplication | Idempotency keys — see [HTTP idempotency](http-idempotency.md) — not an authentication mechanism. |
| Unsigned `X-…-Principal` user header | `OnBehalfOf` token exchange, read back through `actor()`/`subject()` on the receiver. |

## Recorded deviations and deferred items

- **Bearer tokens without sender constraint.** A deviation from RFC 9700's
  SHOULD; accepted for now given short-lived, audience-bound tokens on a
  private network. Reopen if tokens leave the private network, a compliance
  regime requires sender constraint, or a chosen authorization server and a
  maintained Rust client both support DPoP for client credentials.
- **Private network is not identity.** Railway's private network encrypts
  traffic but authenticates no service; tokens, not network location, carry
  identity. Reopen if the platform issues workload identity.
- **DPoP (RFC 9449)** — not adopted: Keycloak supports it since 26.4, but no
  maintained Rust client exists, and it adds a signed proof, nonce state and a retry to
  every token and resource request.
- **mTLS-bound tokens (RFC 8705), SPIFFE, WIMSE** — not adopted: no
  per-service certificates or mesh on the target platform, and WIMSE has no
  mainstream implementation.
- **Platform-issued client assertions (workload identity federation)** — not
  adopted: the caller still holds a long-lived private key. Keycloak 26.6
  accepts a Kubernetes service-account or OIDC token as the client assertion,
  which removes that key, but Railway issues no workload token to a running
  service. Reopen when the platform does.
- **Assertion algorithm.** `RS256` stays the default because every
  shortlisted server accepts it. Prefer `ES256` or `PS256` for a new key: FAPI
  2.0 admits no PKCS#1 v1.5 signatures.
- **Transaction Tokens** — not adopted: still a draft, no shortlisted
  authorization server issues them, and a Txn-Token is not itself an access
  token. Reopen when it is published and a chosen server issues it.

See [Outbound machine authentication
decisions](outbound-machine-authentication-decisions.md) for the underlying
evidence and full reopen conditions.
<!-- template:end outbound-auth:docs-service-to-service-authentication -->
