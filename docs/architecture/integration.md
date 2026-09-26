# Integration Boundaries

Load when a system neighbour, outbound dependency, provider contract, or
cross-service evidence path changes.

| Neighbour | Role on path | Canonical contract | Checkout or clone | Runtime evidence | Owner |
| --- | --- | --- | --- | --- | --- |
| _(none in the template)_ | caller, provider, broker, job, or managed dependency | repository, generated contract, published spec, or live endpoint | path or clone URL | query, dashboard, or command plus the correlation field | accountable team or person |

Record a neighbour when this service calls it, is called by it, or shares
durable state with it. Point to the real contract and the concrete runtime
evidence path joined by the request id or W3C trace context (every log
record inside a request carries `openTelemetry.traceId` and `spanId`). Store
the access shape, never credentials, tokens, or customer data.

## Adding a dependency

<!-- template:begin webhooks-common:docs-integration-webhooks-protocol -->
## Standard Webhooks protocol

The shared provider uses already-resolved aws-lc-rs HMAC and base64 for raw-byte
v1 HMAC-SHA256 framing and library constant-time verification. The published
`standardwebhooks` 1.0.1 crate is not admitted because its UTF-8 input,
uncontrolled clock, overflow-sensitive subtraction, and handwritten comparison
cannot all be repaired by a wrapper. `httpdate` parses outbound Retry-After
HTTP-dates. Reopen only if a published library closes the named gaps or retained
Cargo graphs show a concrete aws-lc backend drawback.
<!-- template:end webhooks-common:docs-integration-webhooks-protocol -->

<!-- template:begin webhooks:docs-integration-webhooks-outbound -->
Outbound webhook delivery reuses `infra-outbound-http` as one bounded,
fixed-authority client per cached origin. The provider passes an admitted saved
origin and original path/query; it does not introduce raw reqwest calls, a
general many-authority transport, proxy handling, redirects, or an inner retry.
<!-- template:end webhooks:docs-integration-webhooks-outbound -->

<!-- template:begin inbound-webhooks:docs-integration-webhooks-inbound -->
Inbound webhook processing crosses to adopter code through a registered consumer
that receives immutable incoming bytes and `&mut Tx`. Database effects and
fenced completion share that transaction; external recipients must be
idempotent. A missing binding snoozes rather than acknowledging work.
<!-- template:end inbound-webhooks:docs-integration-webhooks-inbound -->

Provider adapters live in `crates/infra-<provider>` and own admission,
budgets, retry eligibility, provider errors, and the mapping into
feature-owned types; feature crates depend on the adapter's types through
their own ports, never on the provider's client. Bootstrap owns wiring,
readiness registration, and cleanup. A dynamic or caller-controlled
destination is a separate security decision; a fixed destination comes from
configuration.

Before enabling a runtime dependency, close each of these, in the owner that
enforces it:

| Decision | Owner |
| --- | --- |
| Contract source and its drift or compatibility proof | the adapter crate and [Validation Routing](../validation-routing.md) |
| Trust and egress: destination, TLS, credential source | `crates/config` (typed endpoint, `SecretString`, no ambient credential leakage as the OTLP exporter already enforces) |
| Criticality: does its loss make the instance unable to serve? | a `health` probe registered in bootstrap admission and the refresher; liveness stays process-only |
| Deadline and retry budget inside `http.request_timeout` | the adapter's operation policy ([runtime budgets](../configuration-source-policy.md#runtime-budget-policy)) |
| Partial-startup cleanup and the shutdown stage that closes it | `bootstrap` (`DEPENDENCY_CLOSE`, `5s`, in the teardown plan) |
| Telemetry labels with bounded cardinality | the adapter's instruments, through the `metrics` facade |
| Proof | a negative test at the boundary beside the adapter; container-backed proof behind `ALLOW_HEAVY=1` once a `test/` crate exists |

Available providers are recorded in [Component Boundaries](boundaries.md);
[Persistence](persistence.md) records whether PostgreSQL is retained. A new
outbound or messaging capability needs its own accepted contract and adds its
real owner here when implemented. New executable surfaces
use their own binary in `crates/service/src/bin` only when they share the
service's composition, otherwise their own crate with its own lifecycle.

<!-- template:begin authn:docs-integration-authn-provider -->
Inbound authentication's provider destination is fixed by configuration or an
exact-issuer discovery response, never by the caller. The adapter's one
`ProviderUrl` grammar admits HTTPS, host, and no userinfo, query, fragment,
whitespace, or controls before I/O. It retains normal TLS hostname/certificate
validation, permits configured private HTTPS providers, disables redirects,
ambient proxy and retry, and caps a response at 1 MiB. Provider work has a
three-second attempt cap inside the request's remaining budget.

The adapter owns URL representation because discovery and direct adapter inputs
must pass the same admission. Config owns field presence, type, and useful key
context; bootstrap converts primitive configuration into adapter options. The
independent outbound HTTP profile alone owns post-resolution public-address
admission and its DNS dependency.
<!-- template:end authn:docs-integration-authn-provider -->
<!-- template:begin outbound-http:docs-integration-outbound -->
A provider with a fixed public HTTPS dependency uses the retained
[bounded outbound client](../outbound-http.md). The adapter supplies finite
limits, credentials, a parent deadline with response reserve, and cancellation;
it owns parsing and business errors. The client enforces same-authority targets,
post-DNS public-address admission, bounded encoded bodies/headers and removal
of correlation headers. Selection itself adds no neighbour or startup call.
<!-- template:end outbound-http:docs-integration-outbound -->

<!-- template:begin oidc-jwt:docs-integration-jwt -->
JWT mode uses OIDC discovery and JWKS only from the exact configured issuer's discovery result. It accepts signed RS256 access tokens against eligible RSA keys; token headers never choose a trust destination. Refresh replaces a key set atomically, retains the last usable set after a failed fetch, and is not a revocation service.
<!-- template:end oidc-jwt:docs-integration-jwt -->
<!-- template:begin oidc-introspection:docs-integration-introspection -->
Introspection sends one RFC 7662 POST per admitted cache miss, with the opaque token in form data and `client_secret_basic` credentials. The cache is disabled by default; enabling it permits bounded positive reuse within the same verifier's immutable trust context. Reuse ends at the earlier of the fixed TTL and token expiry, without expiry leeway. A valid hit can delay observing revocation or provider outages until that boundary. Negative results, provider failures, and expired entries never supply cached success; misses keep the same provider/deadline path, with no retry, redirect, or remembered outage. The verifier owns and releases the store without a background task; [Authentication](../authentication.md#oidc-introspection) defines the operator inputs and storage bounds.
<!-- template:end oidc-introspection:docs-integration-introspection -->
