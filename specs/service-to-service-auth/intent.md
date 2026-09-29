# Intent: one standard service-to-service authentication path

## Problem

The GonkaGate and Bitrina backends on Railway, which will be rewritten from
this template, authenticate each other with about six homegrown schemes:
static bearer tokens, HS256 shared secrets, caller-signed RS256 JWTs with keys
copied by hand to each receiver, a custom HMAC request signature with a replay
store (payments), a single-use `jti` (billing), and an unsigned
`X-Bitrina-Principal` user header. The template already verifies inbound JWT
or introspected access tokens, checks scopes in handlers, and acquires
outbound OAuth2 client-credentials tokens with `client_secret_basic` only. It
does not define one complete, standards-based way for services to call each
other, authenticate to the authorization server, or carry user context.

## Desired outcome

Research current standards, best practice, known implementations, and Rust
crates first; record the decisions and crate comparison in the repository's
accepted decision artifact; then implement the selected path as template
profiles so a derived service has exactly one correct way to authenticate
service-to-service calls and to carry user context. Deliver it as one pull
request with green CI.

Working model, which research may refine or reject: a service obtains a
short-lived JWT access token (RFC 9068) from a central authorization server via
OAuth 2.0 client credentials with the receiver's audience; it authenticates to
that server with `private_key_jwt` (RFC 7523), not a shared secret; user
context travels via Token Exchange (RFC 8693) or another signed internal token,
never an untrusted header; each route checks scopes.

## Affected actors and systems

Derived service authors and their agents; the inbound authentication profile
(`crates/infra-bearerauthn`, `infra-http` route policy), the outbound
client-credentials profile (`crates/infra-oauth2-client-credentials`), typed
configuration, the template initializer and markers, guides and decision
records, and CI.

## Scope and non-goals

In scope: research and recorded decisions; the client-authentication method,
key configuration and rotation; user-context propagation and its inbound
verification; per-route scope enforcement; a guide that declares the path the
only supported service-to-service method, forbids static tokens, HS256 and
homegrown signatures, explains choosing and wiring an authorization server,
and describes migration with a temporary dual-acceptance window; tests,
initializer support, and removal of anything the new path replaces.

Non-goals: deploying or selecting a production authorization server, any
Railway change, and any change to `go-service-template-rest` (findings that
apply there are only noted).

## Constraints

Follow the template's optional-profile, initializer, marker, guide and test
structure and its engineering policy (maintained crates before custom code;
replaced paths are deleted, not kept alongside). Railway has no service mesh:
the private network encrypts traffic but does not identify the calling
service, so mTLS/SPIFFE workload identity is assumed unavailable. Push, pull
request, and merge after green CI are authorized for this repository.

## Success signal

The decision record names the selected standards, the rejected alternatives
with evidence, and the crate comparison before code lands. A derived service
can enable the profile, configure a private key instead of a client secret,
propagate user context through the selected standard mechanism, verify it
inbound, and enforce scopes per route, with tests proving each path. The
pull request's CI is green.
