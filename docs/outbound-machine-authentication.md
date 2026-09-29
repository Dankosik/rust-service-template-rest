# Outbound machine authentication

<!-- template:begin outbound-auth:docs-outbound-machine-authentication-guide -->
Stage 10.8 provides default-off private-key client authentication for bounded
outbound integrations, with RFC 8693 token exchange to carry a verified
user's context downstream. [Service-to-service
authentication](service-to-service-authentication.md) states the mandate this
profile implements and forbids the schemes it replaces.
[Decisions](outbound-machine-authentication-decisions.md) own library and lifecycle
choices. This capability does not change inbound authentication.

## Select and configure

`OUTBOUND_AUTH=oauth2-client-credentials` / `--outbound-auth oauth2-client-credentials`
retains the OAuth adapter and its bounded outbound HTTP prerequisite; `none` is
the default. Selection alone adds no provider call, task, readiness dependency,
or listener. The initializer keeps `outbound_http=bounded` as the effective
selection when OAuth is selected, including when its input was `none`; saved
profile state records the effective value. Other profile prerequisites retain
their existing behavior.

Named configuration is a map `integrations.<name>.oauth`. An empty map is inert;
an entry without `oauth` is inert; a present OAuth tuple is complete or startup
fails. Names use the existing config-rs case normalization and `__` environment
path delimiter: use lowercase names without `__` for matching TOML and environment
entries. No separate integration-name slug grammar is imposed.

```toml
[integrations.billing.oauth]
token_url = "https://identity.example/oauth2/token"
client_id = "billing-service"
key_id = "billing-service-2026"
algorithm = "RS256"
assertion_audience = "https://identity.example/"
scopes = ["billing.read"]
# audience = "https://billing-api.example"
```

Supply the private key only through
`APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY`; nonempty file secrets are
rejected by the recursive secret guard. `client_secret` is an unknown key and
fails startup: this profile authenticates only with a private key, never a
shared secret ([mandate](service-to-service-authentication.md#mandate)).
`key_id` and `assertion_audience` are required; `algorithm` is `RS256`
(default), `PS256`, or `ES256` — one algorithm per key. Other keys use normal
file/environment layering. Environment scopes use one space-separated string,
for example `APP__INTEGRATIONS__BILLING__OAUTH__SCOPES=billing.read
billing.write`; TOML uses a list. Omitted or empty scopes send no scope
parameter. Each list member is an RFC 6749 scope token; a configured audience
must be nonempty. Credentials and configuration are immutable; rotate by
restart. Client IDs are nonempty without additional length or character
restrictions; the private key must be a PKCS#8 or PKCS#1 PEM matching
`algorithm`, or construction fails with a sanitized configuration error
naming `private_key` before any I/O happens.

Rotate a key by registering the new public key at the authorization server
beside the old one, deploying the new `private_key` and `key_id` together,
then deleting the old public key. No `jwks_uri` endpoint is served by this
profile.

`token_url` is fixed trusted operator input: HTTPS with host, no userinfo,
fragment, whitespace, or controls. Paths and queries are retained. System DNS
and normal certificate and hostname verification support trusted private,
loopback, and IP providers. There is no public-address filter, discovery,
proxy, redirect, decompression, or internal retry.

## Compose an integration

The retained crate is a workspace member without an unused dependency alias.
When a concrete provider first consumes it, add the dependency once under the
root `[workspace.dependencies]`:

```toml
infra-oauth2-client-credentials = { path = "crates/infra-oauth2-client-credentials" }
```

That provider then declares `infra-oauth2-client-credentials = { workspace = true }`
in its own `[dependencies]`.

`infra-oauth2-client-credentials` owns an opaque cloneable `Credentials`, prepared
from its `Options`. Composition code translates one named config into options,
constructs credentials, and binds them with `credentials.http(resource_client)`.
The resulting cloneable `AuthenticatedClient::execute(request, deadline)` takes
the existing `http::Request<Bytes>` and an absolute `tokio::time::Instant` and
returns the existing bounded response or the OAuth adapter's sanitized error.
There is no public token getter, generic token-source trait, or business client
generator. Each concrete provider adapter receives its own authenticated client;
feature code receives only its existing provider port. Reusing a `Credentials`
clone is an explicit composition decision for the same immutable tuple; separate
constructions never share token state.

No example integration or unused registry is wired into the service. Config
validates every configured tuple during the ordinary snapshot load, including
its endpoint; constructing an authenticated client remains the real integration's
composition-root work. Direct adapter construction repeats admission at that
independent public boundary rather than trusting arbitrary caller options.

To call an integration on behalf of a verified user instead of as the
service itself, attach `OnBehalfOf::new(principal.access_token().clone())` to
the outbound request's extensions
(`http::Request::extensions_mut`) before calling `execute`; the gRPC binding
takes the same value through `tonic::Request::extensions_mut`. With it
present, `execute`/`call` exchange the carried token for one addressed to
this integration (see [Token exchange](#token-exchange-for-user-context))
instead of sending the service's own token; without it, the existing
service-token path below runs unchanged.

An existing Authorization header is refused before token or resource I/O,
whether or not `OnBehalfOf` is attached. Otherwise acquisition supplies
exactly one sensitive Bearer header. The
resource client's origin check, body limit, transport policy, and original
absolute caller deadline remain authoritative. Token wait consumes that deadline;
it never resets it. Completed resource results, including 401 and 403, pass
through without replay. A 401 evicts the credential that request used, unless a
newer one already replaced it, so the next operation acquires a fresh token; a
403 keeps it.

## Acquisition and reuse

The first call signs a fresh client assertion and posts the RFC 6749
client-credentials form with `grant_type=client_credentials`, `client_id`,
`client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`,
and `client_assertion`; there is no `Authorization` header. Scopes and
audience are sent only when configured. Each token attempt has one
five-second cap through body completion, narrowed by its initiating caller's
remaining deadline. The token transport has one active exchange, 64 response
headers, and a 1 MiB encoded body maximum. The shared transport enforces
header count, not a configurable aggregate header-byte limit. These are
implementation constants, not operator tuning keys.

The assertion header is `{alg, kid, typ: "client-authentication+jwt"}`; its
claims are `iss` = `sub` = the client ID, `aud` = the configured
`assertion_audience` as one JSON string, `iat` = `nbf` = now, `exp` = now +
60 seconds, and `jti` = a fresh random UUID v4. A new assertion is signed for
every token request and never cached or reused; two requests never share a
`jti`.

One owner never runs two token requests at once. Callers that arrive while a
request is in flight wait for it, then reuse its token if it is reusable. After
a failure, each waiting caller makes its own request in turn. Every caller
bounds its own wait by its own absolute deadline. Dropping the requesting future
cancels its exchange and lets the next waiter proceed. Dropping the last
client/owner releases the cached token.

Once a quarter of the reuse period, or at most five minutes, remains before the
reuse cutoff, the first caller to find the token also starts one detached
refresh, as Azure.Core refreshes five minutes early. No caller waits for it
while the current token is reusable: every caller keeps that token until the
new one is stored. The attempt has only the five-second cap and takes the
refresh lock like any other request. A failure keeps the current token until its
reuse cutoff. After either outcome the next background attempt waits at least
thirty seconds. The attempt is not joined at shutdown; the runtime cancels it.

A positive `expires_in` establishes a conservative monotonic expiry from
acquisition start. Keep the Go ten-second margin as a *reuse cutoff*: reuse the
token only until expiry minus ten seconds. A token that is still valid but
already within that margin, including any lifetime at most ten seconds, serves
only the request that fetched it. This avoids rejecting short-lived valid
responses.

A missing or unrepresentably large lifetime has no reuse cutoff: as in Go's
`oauth2`, the token is reused until a resource 401 evicts it. Zero lifetime or a
token already past its expiry when the response arrives cannot authorize
dispatch. Hits never slide expiry. Failed attempts are not cached, and no token
is reused past its cutoff. A later operation may fetch again. Unknown response
fields and refresh tokens are discarded; JWT claims are not interpreted. Only
case-insensitive Bearer tokens that are nonempty and form a valid header value
are admitted. A present Content-Type must be `application/json`; there is no
custom TTL ceiling or stricter JSON or duplicate-field rule.

## Token exchange for user context

With `OnBehalfOf` attached (see [Compose an
integration](#compose-an-integration)), acquisition posts
`grant_type=urn:ietf:params:oauth:grant-type:token-exchange`,
`subject_token` (the carried token), `subject_token_type` and
`requested_token_type` both
`urn:ietf:params:oauth:token-type:access_token`, the integration's configured
`scope`/`audience`, and the same client assertion fields as client
credentials. The response is admitted only with a Bearer `token_type`, a
nonempty token that forms a header value, and `issued_token_type` equal to
the access-token type URI; anything else is an invalid-response failure and
no token is used.

Exchanged tokens are cached per credential owner, keyed by the SHA-256 digest
of the subject token, bounded to 1024 entries, each until its own reuse
cutoff (the same ten-second margin as above). Concurrent requests for one
subject share a single in-flight exchange. A resource 401 evicts only that
subject's cache entry, not the whole cache. An exchanged token with no
`expires_in` serves only the request that fetched it and is never stored;
failed exchanges are never cached.

## Failure and observation

Configuration errors identify the integration/key with a bounded static reason,
never its value. A key that is not PEM, or that does not match `algorithm`,
fails construction with a sanitized configuration error naming
`private_key`; no I/O happens. Runtime token failures use closed reasons for
deadline, transport, response limit, provider unavailability (5xx), provider
rejection, invalid response, and `assertion` for an assertion-signing
failure. Resource
transport errors remain distinct; concrete integrations own business/HTTP error
mapping. No inbound Problem code is added.

Raw OAuth errors can contain provider bytes and must be consumed and discarded
inside the adapter. Debug, Display, error sources, metrics, and logs expose no
credentials, tokens, scope/audience values, response bodies, endpoint path/query,
or arbitrary provider text. Credential, option, cache, and authenticated-client
Debug implementations are redacted. Token attempt metrics are
`oauth2_token_acquisitions_total{grant, outcome}`, with `grant` in
`client_credentials | token_exchange` and a finite outcome label; there is no integration/URL label. Existing safe
outbound transport observation remains enabled. Cancellation records an
outcome without claiming a provider result. No new diagnostic route or
body/header logging is introduced.

## Documented provider compatibility

| Provider | Required registration and request choices |
| --- | --- |
| [Keycloak](https://www.keycloak.org/docs/latest/server_admin/#_service_accounts) | Confidential client, client authenticator "Signed JWT" with the service's public key or JWKS URL, service account enabled; standard token exchange V2 (GA since 26.2) supports `OnBehalfOf`. |
| [Okta custom authorization server](https://developer.okta.com/docs/guides/implement-grant-type/clientcreds/main/) | Service application with a registered public key (`private_key_jwt`) and the custom authorization-server token endpoint as `assertion_audience`; per-service audiences need the paid API Access Management add-on; `act` in exchanged tokens is undocumented. |
| [Auth0 (Enterprise)](https://auth0.com/docs/get-started/authentication-and-authorization-flow/client-credentials-flow/call-your-api-using-the-client-credentials-flow) | `private_key_jwt` client authentication is an Enterprise-plan feature; configure the M2M application's public key and audience. |
| [Spring Authorization Server](https://docs.spring.io/spring-authorization-server/reference/) | JWT client assertion authentication is supported protocol-natively; `act` emission and audience mapping on exchange are operator-owned custom code. |
| Hydra | Supports `private_key_jwt` client credentials; has no RFC 8693 token exchange (issue open since 2018), so `OnBehalfOf` is unavailable. |
| Cognito — unsupported | Secret-only client authentication; does not support `private_key_jwt`. |
| Entra — unsupported | Requires an `x5t#S256` certificate-thumbprint header this profile does not send, and offers no token exchange that emits `act`. |

These are official-documentation compatibility findings, not live-provider
certification. Adopters own registration, grants/scopes, credentials, rotation,
network/TLS policy, capacity, readiness criticality, and live-provider acceptance.
See [Choosing an authorization
server](service-to-service-authentication.md#choosing-an-authorization-server)
for the full shortlist and the evidence behind it. Other authentication
methods require a separate accepted behavior decision.

<!-- template:end outbound-auth:docs-outbound-machine-authentication-guide -->
<!-- template:begin outbound-auth-grpc:docs-oauth-grpc-binding -->
With `GRPC=enabled`, `Credentials::grpc` binds the same private acquisition owner
to an `infra_grpc::Client`. Each call spends `grpc-timeout` when that header is
present, otherwise the owner's fetch timeout. `OnBehalfOf` set on the call's
extensions through `tonic::Request::extensions_mut` selects the same token
exchange as the HTTP binding; without it, the service token is sent. Token
failure prevents resource
dispatch. Eviction inspects only the initial response and never replays the
call. When acquisition waited at least a millisecond, `grpc-timeout` is
rewritten to the remaining budget; a reused token forwards it unchanged.
Streaming acquires once at opening. The [gRPC
guide](grpc.md#reuse-clients-and-original-deadlines) shows the concrete binding.
Removing either profile removes only the combined bridge.
<!-- template:end outbound-auth-grpc:docs-oauth-grpc-binding -->
