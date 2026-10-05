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
algorithm = "ES256"
assertion_audience = "https://identity.example/"
scopes = ["billing.read"]
# audience = "https://billing-api.example"
# exchange_cache_capacity = 1024
# provider_concurrency = 32
```

Supply the private key only as the variable
`APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY`: in the process environment
or, which keeps a multi-line PEM whole, as a file of that name in the
[secrets directory](configuration-source-policy.md#secrets-directory). A
nonempty `private_key` in a TOML file is rejected by the recursive secret
guard. `client_secret` is an unknown key and
fails startup: this profile authenticates only with a private key, never a
shared secret ([mandate](service-to-service-authentication.md#mandate)).
`key_id`, `assertion_audience`, and `algorithm` are required; `algorithm` is
`RS256`, `PS256`, or `ES256` — one algorithm per key, with no default, so a
tuple that omits it fails startup naming the key. Prefer `ES256` or `PS256`
for a new key and keep `RS256` for a server that accepts nothing else. Other
keys use normal file/environment layering. Environment scopes use one space-separated string,
for example `APP__INTEGRATIONS__BILLING__OAUTH__SCOPES=billing.read
billing.write`; TOML uses a list. Omitted or empty scopes send no scope
parameter. Each list member is an RFC 6749 scope token; a configured audience
must be nonempty. `exchange_cache_capacity` (default 1024, inclusive 1–65536)
bounds how many subjects keep a token exchanged on their behalf; see [Token
exchange](#token-exchange-for-user-context). `provider_concurrency` defaults to 32
and accepts a positive `u32` integer or integer string; zero, fractions,
booleans, negatives, and overflow fail startup. It bounds actual token attempts
across both grants, independently of retained cache entries. Credentials and configuration
are startup snapshots; replacing a secret file does not replace the admitted
assertion signing key or `key_id`. Rotate them by restart. Client IDs are nonempty without additional length or character
restrictions; the private key must be a PKCS#8 or PKCS#1 PEM matching
`algorithm`, or construction fails with a sanitized configuration error
naming `private_key` before any I/O happens.

Access-token refresh below does not rotate the client assertion signing key.
For signing-key rotation, register the new public key at the authorization
server beside the old one, then deploy the new `private_key` and `key_id`
together through process replacement. Keep provider overlap until the transition
has completed and fresh token acquisition succeeds with the new key. Already
issued access tokens have their own lifetime and provider revocation policy;
removing the old client public key does not by itself revoke them. Account for
those independently valid tokens before retiring old material under the
provider's policy. Emergency revocation uses the provider's token/session
controls as well as the required restart. No `jwks_uri` endpoint is served by
this profile.

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

`infra-oauth2-client-credentials` owns an opaque cloneable `Credentials` and a
non-cloneable, must-use `RefreshDriver`. Composition translates one named config
into options and prepares both without I/O:

```rust,ignore
let (credentials, driver) = Credentials::prepare(options)?;
let client = credentials.http(resource_client);
```

The concrete integration owns `driver.run(shutdown)`, where `shutdown` is its
existing cancellation future. It drives that future for as long as credentials
or authenticated-client clones are in use, and awaits its completion in the
existing background-join stage before dropping dependencies. That expected
completion is distinct from an unexpected background-task exit in a process
root. For a shorter integration lifetime, dropping the final
`Credentials`/client owner also completes the driver; a surviving client is
closed if its driver is dropped. A detached `spawn` is not a completion owner.

For a scoped integration, join the work and the driver. The work future owns
the final client clone, so its return drops that clone and lets the pending
driver return:

```rust,ignore
let (credentials, driver) = Credentials::prepare(options)?;
let client = credentials.http(resource_client);
drop(credentials);
let work = async move { run_integration(client).await };
let (result, ()) = tokio::join!(work, driver.run(std::future::pending()));
result
```

For a process-lifetime integration, an existing lifecycle owner may spawn the
driver only while it retains its handle. Clone the existing cancellation token
into the `'static` driver future. When service work returns, whether normally or
with an error, cancel that token. Await the expected `Ok(())` under the existing
`background_join_deadline` before dropping dependencies; a `JoinError` remains
an unexpected task failure. On that existing deadline, abort the retained handle
and await it before recording degraded shutdown and dropping dependencies:

```rust,ignore
let driver_shutdown = shutdown.clone();
let mut refresh_driver = tokio::spawn(async move {
    driver.run(driver_shutdown.cancelled()).await;
});
let service_result = serve_until_shutdown(client, &shutdown).await;
shutdown.cancel();
let degraded = match tokio::time::timeout_at(background_join_deadline, &mut refresh_driver).await {
    Ok(Ok(())) => false, // expected after shutdown or final client-owner release
    Ok(Err(error)) => return Err(report_unexpected_background_exit(error)),
    Err(_) => {
        refresh_driver.abort();
        let _ = refresh_driver.await;
        true
    }
};
drop(dependencies);
if degraded {
    return Err(report_degraded_shutdown());
}
service_result
```
The resulting cloneable `AuthenticatedClient::execute(request, deadline)` takes
the existing `http::Request<Bytes>` and an absolute `tokio::time::Instant` and
returns the existing bounded response or the OAuth adapter's sanitized error.
There is no public token getter, generic token-source trait, or business client
generator. Each concrete provider adapter receives its own authenticated client;
feature code receives only its existing provider port. Reusing a `Credentials`
clone is an explicit composition decision for the same immutable tuple; separate
constructions never share token state or provider capacity. Transfer the configured
limit at the same composition boundary as the other fields:

```rust,ignore
use infra_oauth2_client_credentials::{Algorithm, Options};
use service_config::OAuthAlgorithm;

let options = Options {
    token_url: oauth.token_url.clone(),
    client_id: oauth.client_id.clone(),
    private_key: oauth.private_key.clone(),
    key_id: oauth.key_id.clone(),
    algorithm: match oauth.algorithm {
        OAuthAlgorithm::Rs256 => Algorithm::Rs256,
        OAuthAlgorithm::Ps256 => Algorithm::Ps256,
        OAuthAlgorithm::Es256 => Algorithm::Es256,
    },
    assertion_audience: oauth.assertion_audience.clone(),
    scopes: oauth.scopes.as_slice().to_vec(),
    audience: oauth.audience.clone(),
    exchange_cache_capacity: oauth.exchange_cache_capacity,
    provider_concurrency: oauth.provider_concurrency,
};
```

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

A request that should have carried `OnBehalfOf` and did not is therefore sent
with the service's own authority. Bind an integration that only ever acts for
a user with `credentials.http(resource_client).require_on_behalf_of()` (the
gRPC binding has the same method): such a client refuses a request without
`OnBehalfOf` before any token or resource I/O, as `Error::SubjectRequired`
or gRPC `INTERNAL`, and never sends the service token.

An existing Authorization header is refused before token or resource I/O,
whether or not `OnBehalfOf` is attached. Otherwise acquisition supplies
exactly one sensitive Bearer header. The
resource client's origin check, body limit, transport policy, and original
absolute caller deadline remain authoritative. Token wait consumes that deadline;
it never resets it. Completed resource results, including 401 and 403, pass
through without replay. A 401 evicts the credential that request used once it
is at least thirty seconds old, unless a newer one already replaced it, so the
next operation acquires a fresh token; a 403 keeps it. A younger token stays:
the provider would issue an equivalent one, so a resource that refuses every
token costs one token request per thirty seconds, not one per call.

## Acquisition and reuse

The first call signs a fresh client assertion and posts the RFC 6749
client-credentials form with `grant_type=client_credentials`, `client_id`,
`client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`,
and `client_assertion`; there is no `Authorization` header. Scopes and
audience are sent only when configured. Each token attempt has one
five-second cap through body completion, narrowed by its initiating caller's
remaining deadline. The token transport admits 64 response headers and a
1 MiB encoded body. The shared transport enforces
header count, not a configurable aggregate header-byte limit. These are
implementation constants, not operator tuning keys.

The assertion header is `{alg, kid, typ: "client-authentication+jwt"}`; its
claims are `iss` = `sub` = the client ID, `aud` = the configured
`assertion_audience` as one JSON string, `iat` = `nbf` = ten seconds before
now, `exp` = `iat` + 60 seconds, and `jti` = a fresh random UUID v4. Dating it
back, as Go's `oauth2/jws` does, keeps a provider whose clock is slightly
behind from reading it as issued in the future. A new assertion is signed for
every token request and never cached or reused; two requests never share a
`jti`.

One owner never runs two service-token requests at once; [token
exchange](#token-exchange-for-user-context) is coalesced per subject instead.
Callers that arrive while service acquisition is in flight wait under their
own absolute deadline, then independently re-evaluate the reusable token and
completed-failure record. A reusable token always wins over that record. A
completed provider refusal, transport/limit/unavailable/invalid-response or
assertion failure, and a full five-second adapter timeout, are shared for one
second without another token request; success clears the record. Cancellation,
lock waiting, and a caller deadline shorter than the five-second attempt do
not install it, so another caller remains eligible under its own budget.
Dropping a requesting future cancels its request and lets the next waiter
proceed. Expiry or an eligible 401 eviction does not erase a completed failure
record. Dropping the last client/owner releases cached state.

After cache lookup and same-key coalescing, each actual initializer tries to
acquire one shared provider slot before signing or token I/O. Saturation returns
`AcquisitionError::AtCapacity` immediately, with no admission queue, reservation,
priority, automatic retry, or cached failure. An already expired deadline wins
over capacity refusal. Hits and coalesced waiters use no slot, and callers keep
their original absolute budgets. The permit lasts through body completion,
parsing, and token admission, then releases on success, error, timeout, or drop.
Cancelling a waiter cannot release another caller's live slot; a replacement
initializer after leader cancellation must acquire its own. Local cancellation
does not promise physical cancellation of provider work or system DNS.

At default capacity, at most 32 active token attempts can hold response bodies
with the 1 MiB payload ceiling each. Requested accumulator storage is at most
32 MiB between growths, or 64 MiB allowing concurrent relocation, plus current
transport frames/read buffers, parser and token storage. Cache storage and
allocator/RSS costs are separate; these are not measured memory or throughput
claims. See the [transport bounds](outbound-http.md#deadline-transport-and-retry-ownership).

When a reusable service token is admitted, its refresh lead is sampled once
between 90% and 100% of `min(5 minutes, reusable lifetime / 4)`. The reusable
lifetime runs from acquisition start to the reuse cutoff. The first caller at
or after cutoff minus that lead queues one refresh for the owned
`RefreshDriver`. Cache hits do not resample or slide this eligibility; an idle
client starts no autonomous refresh. No caller waits while the current token is
reusable: every caller keeps that token until the new one is stored.

The attempt's five-second cap starts at enqueue and includes waiting for
acquisition ownership and the network operation. Enqueue independently samples
a 30–33 second spacing for the next background eligibility, including when the
queue refuses the attempt. Successful completion samples another 30–33 second
spacing and retains the later of that deadline and the replacement token's own
eligibility. Failure keeps the enqueue spacing and current token until its
reuse cutoff, including a provider-capacity refusal. A completed provider
failure is shared for one second; success clears that record. Once reuse ends,
a foreground miss retains its immediate bounded acquisition behavior.

The existing AWS-LC random source supplies each sample. If it fails, the lead
uses the full maximum and retry spacing uses thirty seconds. There is no new
readiness failure or diagnostic. Driver shutdown or final-owner loss cancels
queued or active refresh work and its awaited return establishes completion.
Missing expiry and non-reusable short tokens gain no background work; token
exchange retains its cache behavior without background refresh.

A positive `expires_in` establishes a conservative monotonic expiry from
acquisition start. Keep the Go ten-second margin as a *reuse cutoff*: reuse the
token only until expiry minus ten seconds. A token that is still valid but
already within that margin, including any lifetime at most ten seconds, serves
only the request that fetched it. This avoids rejecting short-lived valid
responses.

A missing `expires_in` permits the acquisition that received it to dispatch
once, but never enters either cache; a later call acquires again. A present
zero, expired, or unrepresentably large lifetime is an invalid response and
cannot authorize dispatch. Hits never slide expiry. Token and exchange failures
are never cached; service acquisition has the separate one-second completed
failure record above. No token is reused past its cutoff. A later operation may
fetch again. Unknown response fields and refresh tokens are discarded; JWT
claims are not interpreted.
`expires_in` is a JSON number of whole seconds, as RFC 6749 section 5.1
defines it; a response that sends it as a string is an invalid response. Only
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
of the subject token, until their reuse cutoff (the same ten-second margin as
above). Settled retention targets both `exchange_cache_capacity` entries
(default 1024) and 16 MiB of Bearer-token bytes: cache weight applies the
tighter target to each entry. Both are best-effort retention targets, not
instantaneous admission or a process-memory/RSS ceiling; concurrent insertions,
active calls, keys, metadata and allocator overhead can temporarily exceed
them. Past settled retention, a subject without a retained token is exchanged
on every call, which `oauth2_token_acquisitions_total{grant="token_exchange"}`
shows as a rate tracking the request rate; size the count target to users active
on one replica within a token lifetime. Concurrent requests for one
subject share a single in-flight exchange: the first caller's, bounded by
that caller's deadline and the five-second cap. A failure is returned to
every caller that waited for it. When the first caller is dropped or out of
budget, its exchange is cancelled and a waiting caller starts its own, so no
caller fails because of another's deadline. Exchanges for different subjects
run concurrently within the same owner's `provider_concurrency`, shared with
service-token and background-refresh attempts. A capacity refusal neither
substitutes a service token nor evicts another subject's cached token. A resource 401 evicts only that
subject's cache entry, not the whole cache. An exchanged token with no
`expires_in` serves only the request that fetched it and is never stored;
failed exchanges are never cached.

## Failure and observation

Configuration errors identify the integration/key with a bounded static reason,
never its value. A request refused before any I/O is a caller `Authorization`
conflict or, on a client bound with `require_on_behalf_of()`, a missing
subject. A key that is not PEM, or that does not match `algorithm`,
fails construction with a sanitized configuration error naming
`private_key`; no I/O happens. Runtime token failures use closed reasons for
deadline, local provider capacity, transport, response limit, provider unavailability (5xx or 429),
provider rejection, invalid response, and `assertion` for an assertion-signing
failure. A rejection carries a closed `Rejection`: the response's `error` code
when it is `invalid_request`, `invalid_client`, `invalid_grant`,
`unauthorized_client`, `unsupported_grant_type`, `invalid_scope`, or
`invalid_target` (RFC 6749 section 5.2, RFC 8693 section 2.2.2), otherwise
`Other`. A provider reports an unusable `OnBehalfOf` token as
`invalid_request` (RFC 8693), the same code as a malformed request, so that
code alone does not prove the user's token expired. Resource
transport errors remain distinct; concrete integrations own business/HTTP error
mapping. No inbound Problem code is added.

Raw OAuth errors can contain provider bytes: only a registered `error` code
survives, as its closed variant, and everything else is consumed and discarded
inside the adapter. Debug, Display, error sources, metrics, and logs expose no
credentials, tokens, scope/audience values, response bodies, endpoint path/query,
or arbitrary provider text. Credential, option, cache, and authenticated-client
Debug implementations are redacted. Token attempt metrics are
`oauth2_token_acquisitions_total{grant, outcome}`, with `grant` in
`client_credentials | token_exchange` and a finite outcome label; there is no integration/URL label.
A rejection's outcome is its registered error code, or `rejected` for `Other`.
Capacity refusal emits `capacity` once per rejected initializer; coalesced
waiters add no observations, and a refusal emits no outbound-HTTP attempt.
HTTP callers receive the closed acquisition error before resource dispatch;
gRPC maps capacity to `UNAVAILABLE` with `client credentials unavailable` and
the typed local error source. Direct Rust `Options` literals must add
`provider_concurrency`, and exhaustive acquisition-error matches must handle
`AtCapacity`; existing configuration files retain the default.
Existing safe
outbound transport observation remains enabled. Cancellation records an
outcome without claiming a provider result. A caller receives every token
failure as its error and logs it at its own boundary; only the background
refresh has no caller, so its failure is the one event this adapter logs:
`oauth2_background_refresh_failed` at `WARN` with `server.address` (the token
endpoint host) and `error.type` (the outcome label). No new diagnostic route or
body/header logging is introduced.

## Documented provider compatibility

| Provider | Required registration and request choices |
| --- | --- |
| [Keycloak](https://www.keycloak.org/docs/latest/server_admin/#_service_accounts) | Confidential client, client authenticator "Signed JWT" with the service's public key or JWKS URL, service account enabled; standard token exchange V2 (GA since 26.2) supports `OnBehalfOf`. A key registered as a JWKS must declare `use: sig`. Proven against 26.8.0 by the Keycloak suite below. |
| [Okta custom authorization server](https://developer.okta.com/docs/guides/implement-grant-type/clientcreds/main/) | Service application with a registered public key (`private_key_jwt`) and the custom authorization-server token endpoint as `assertion_audience`; per-service audiences need the paid API Access Management add-on; `act` in exchanged tokens is undocumented. |
| [Auth0 (Enterprise)](https://auth0.com/docs/get-started/authentication-and-authorization-flow/client-credentials-flow/call-your-api-using-the-client-credentials-flow) | `private_key_jwt` client authentication is an Enterprise-plan feature; configure the M2M application's public key and audience. |
| [Spring Authorization Server](https://docs.spring.io/spring-authorization-server/reference/) | JWT client assertion authentication is supported protocol-natively; `act` emission and audience mapping on exchange are operator-owned custom code. |
| Hydra | Supports `private_key_jwt` client credentials; has no RFC 8693 token exchange (issue open since 2018), so `OnBehalfOf` is unavailable. |
| Cognito — unsupported | Secret-only client authentication; does not support `private_key_jwt`. |
| Entra — unsupported | Requires an `x5t#S256` certificate-thumbprint header this profile does not send, and offers no token exchange that emits `act`. |

`ALLOW_HEAVY=1 make test-integration-oauth` runs the adapter against a
throwaway Keycloak container: `RS256`, `PS256`, and `ES256` assertions, a
service token, a token exchanged for a real subject token and addressed to the
configured audience, and the error codes Keycloak reports for an unregistered
key, an unknown scope, and an unusable subject token. CI runs it as the
`oauth_integration` surface. The other rows
are official-documentation compatibility findings, not live-provider
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
exchange as the HTTP binding; without it, the service token is sent, unless
the client was bound with `require_on_behalf_of()`, which answers
`INTERNAL`, as a caller-supplied `Authorization` does: both are this
service's own composition mistakes, never its caller's. Token
failure prevents resource
dispatch: `DEADLINE_EXCEEDED` when the budget ran out, `UNAVAILABLE` when the
provider could not be reached or answered 5xx or 429, and `UNAUTHENTICATED`
when it refused the request or answered unusably. The closed
`AcquisitionError` is the status source. Eviction inspects only the initial response and never replays the
call. When acquisition waited at least a millisecond, `grpc-timeout` is
rewritten to the remaining budget; a reused token forwards it unchanged.
Streaming acquires once at opening. The [gRPC
guide](grpc.md#reuse-clients-and-original-deadlines) shows the concrete binding.
Removing either profile removes only the combined bridge.
<!-- template:end outbound-auth-grpc:docs-oauth-grpc-binding -->
