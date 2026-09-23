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
Inbound authentication's provider destination is fixed by validated configuration, never by the caller. The adapter permits only HTTPS, normal TLS hostname/certificate validation, public-unicast connection destinations, no redirects, proxy, retries, or connection reuse, and a 1 MiB response body. Provider work is bounded by three seconds and the enclosing request deadline; the HTTP transport's existing inbound header limit is separate from the provider-client header-count limit.
<!-- template:end authn:docs-integration-authn-provider -->
<!-- template:begin oidc-jwt:docs-integration-jwt -->
JWT mode uses OIDC discovery and JWKS only from the exact configured issuer's discovery result. It accepts signed RS256 access tokens against eligible RSA keys; token headers never choose a trust destination. Refresh replaces a key set atomically, retains the last usable set after a failed fetch, and is not a revocation service.
<!-- template:end oidc-jwt:docs-integration-jwt -->
<!-- template:begin oidc-introspection:docs-integration-introspection -->
Introspection sends exactly one RFC 7662 POST for each admitted request, with the opaque token in form data and `client_secret_basic` credentials. It has no cache, retry, redirect, or remembered outage; a provider failure affects that request only.
<!-- template:end oidc-introspection:docs-integration-introspection -->
