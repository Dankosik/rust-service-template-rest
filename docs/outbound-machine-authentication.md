# Outbound machine authentication

<!-- template:begin outbound-auth:docs-outbound-machine-authentication-guide -->
Stage 10.8 provides default-off OAuth2 client-credentials authentication for
bounded outbound integrations.
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
scopes = ["billing.read"]
# audience = "https://billing-api.example"
```

Supply the secret only through
`APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_SECRET`; nonempty file secrets are
rejected by the recursive secret guard. Other keys use normal file/environment
layering. Environment scopes use one space-separated string, for example
`APP__INTEGRATIONS__BILLING__OAUTH__SCOPES=billing.read billing.write`; TOML uses
a list. Omitted or empty scopes send no scope parameter. Each list member is
an RFC 6749 scope token; a configured audience must be nonempty. Credentials
and configuration are immutable; rotate by restart. Client IDs and secrets
are nonempty without additional length or character restrictions.

`token_url` is fixed trusted operator input: HTTPS with host, no userinfo,
fragment, whitespace, or controls. Paths and queries are retained. System DNS
and normal certificate and hostname verification support trusted private,
loopback, and IP providers. There is no public-address filter, discovery,
proxy, redirect, decompression, or internal retry.

## Compose an integration

`infra-oauth2-client-credentials` owns an opaque cloneable `Credentials`, prepared
from its `Options`. Composition code translates one named config into options,
constructs credentials, and binds them with `credentials.http(resource_client)`.
The resulting cloneable `AuthenticatedClient::execute(request, operation)` takes
the existing `http::Request<Bytes>` and `infra_outbound_http::Operation` and
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

An existing Authorization header is refused before token or resource I/O.
Otherwise acquisition supplies exactly one sensitive Bearer header. The
resource client's target, admission, body limit, transport policy, and original
absolute caller deadline remain authoritative. Token wait consumes that deadline;
it never resets it. Completed resource results, including 401 and 403, pass
through without token invalidation or replay.

## Acquisition and reuse

The first call posts the RFC 6749 client-credentials form with Basic client
authentication. Scopes and audience are sent only when configured. Each token
attempt has one five-second cap through body completion, narrowed by its
initiating caller's remaining deadline. The token transport has one active
exchange, 64 response headers, and a 1 MiB encoded body maximum. The shared
transport enforces header count, not a configurable aggregate header-byte limit.
These are implementation constants, not operator tuning keys.

Concurrent callers on one owner share a pending result, including failure.
Every caller bounds its own wait by its own absolute deadline. Dropping the
initiating future cancels its exchange; surviving waiters may elect a replacement
under their own budgets. There is no detached fetch or application maintenance
task. Dropping the last client/owner releases cache ownership.

A positive `expires_in` establishes a conservative monotonic hard expiry from
acquisition start. No resource dispatch may use a token at or beyond that
boundary. Keep the Go ten-second margin as a *reuse cutoff*: cache only until
hard expiry minus ten seconds. A token that is still valid but already within
that margin, including any lifetime at most ten seconds, may serve the current
acquisition's waiters and is not retained. This avoids rejecting short-lived
valid responses. No proactive refresh is started.

Missing expiry and an unrepresentably large lifetime use the same successful,
non-retained path. Zero lifetime or a token already past known hard expiry
cannot authorize dispatch. Hits never slide expiry. Failed attempts are not
cached and never fall back to an older token. A later operation may fetch again.
Unknown response fields and refresh tokens are discarded; JWT claims are not
interpreted. Only case-insensitive Bearer tokens that can safely form the RFC
6750 Authorization value are admitted. No custom TTL ceiling or stricter JSON,
duplicate-field, or media-type parser is added around the protocol library.

## Failure and observation

Configuration errors identify the integration/key with a bounded static reason,
never its value. Runtime token failures use closed reasons for deadline,
transport, response limit, provider rejection, and invalid response. Resource
transport errors remain distinct; concrete integrations own business/HTTP error
mapping. No inbound Problem code is added.

Raw OAuth errors can contain provider bytes and must be consumed and discarded
inside the adapter. Debug, Display, error sources, metrics, and logs expose no
credentials, tokens, scope/audience values, response bodies, endpoint path/query,
or arbitrary provider text. Credential, option, cache, and authenticated-client
Debug implementations are redacted. Token attempt metrics use only a finite
outcome label and no integration/URL label; existing safe outbound transport
observation remains enabled. Cancellation records an outcome without claiming a
provider result. No new diagnostic route or body/header logging is introduced.

## Documented provider compatibility

| Provider | Required registration and request choices |
| --- | --- |
| [Entra v2](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-client-creds-grant-flow) | Tenant token endpoint, client secret Basic, one resource URI `/.default` scope; no legacy resource parameter. |
| [Okta custom authorization server](https://developer.okta.com/docs/guides/implement-grant-type/clientcreds/main/) | Service application, Basic, custom authorization-server token endpoint and granted scope. Org-server service apps requiring private-key JWT are outside this profile. |
| [Keycloak](https://www.keycloak.org/docs/latest/server_admin/#_service_accounts) | Confidential client, service account enabled, assigned roles/client scopes, realm token endpoint, Basic. |
| [Auth0](https://auth0.com/docs/get-started/authentication-and-authorization-flow/client-credentials-flow/call-your-api-using-the-client-credentials-flow) | M2M application granted the API, configured audience, optional scopes, application authentication method Client Secret Basic. |
| [Cognito](https://docs.aws.amazon.com/cognito/latest/developerguide/token-endpoint.html) | Client credentials enabled, client secret, custom resource-server scopes, domain token endpoint. |

These are official-documentation compatibility findings, not live-provider
certification. Adopters own registration, grants/scopes, credentials, rotation,
network/TLS policy, capacity, readiness criticality, and live-provider acceptance.
Other authentication methods require a separate accepted behavior decision.

The HTTP surface is complete in this stage. Stage 10.7 owns gRPC transport;
whichever stage merges second composes credentials there and proves the combined
behavior. Keep the token private inside infrastructure; do not add a speculative
public authorizer now. gRPC composition may add a concrete adapter in this crate
when the real transport exists.
<!-- template:end outbound-auth:docs-outbound-machine-authentication-guide -->
