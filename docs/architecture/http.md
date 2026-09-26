# HTTP Architecture

Load for an HTTP contract, route, middleware, exposure, or handler-composition
change. The decisions behind the request path and the contract, with the
alternatives they beat, are recorded at the end of this document.

## Request path

1. `bootstrap` in `crates/service` builds configuration, telemetry, and
   readiness, takes the route tree from `service::api::contract()`, wraps it
   in `infra_http::harden`, and binds it with `infra_http::Server`.
2. The bounded server (`crates/infra-http/src/server.rs`) owns
   connection-level policy: the connection cap, the header timeout that
   doubles as the keep-alive idle bound, the header-size bound (`431` as
   `text/plain`, before routing), the silent-client guard, and the graceful
   drain.
3. The hardened chain (`crates/infra-http/src/harden.rs`) owns request-level
   policy, outermost first: request-id admission and propagation, `nosniff`,
   the OpenTelemetry server span, HTTP metrics, the access log, error mapping
   (`503` shedding with `Retry-After`, `504` timeout with code
   `request_timeout`), load shedding, the in-flight limit, the request timeout,
   panic recovery (`500`), and the body
   limit (`413`). Every layer is applied with `Router::layer`, so the `404`
   and `405` fallbacks travel through the same chain. There is no CORS
   layer: browser cross-origin requests are fail-closed by omission.
4. The route tree is one `infra_http::ContractRouter`.
   `infra_http::router()` contributes the platform probes and the problem
   components; feature routers merge with it in `service::api::contract()`.
   One finalizer consumes the carrier into the served axum `Router`; the
   document-only path is separate, so the two cannot describe different
   services.
5. Handlers return the typed responses their contract declares (an
   `IntoResponses` enum with one variant per status, or a `Problem`).
   Failures are `Problem` values from the closed catalog in
   `infra_http::problem`, rendered as `application/problem+json` with the
   request id; nothing submitted by the caller is echoed.
6. Edge observability uses route templates, not raw paths; unmatched requests
   carry an explicit label. `/metrics` stays on the separate diagnostics
   listener owned by
   [Configuration Source Policy](../configuration-source-policy.md#opentelemetry-environment-policy),
   which binds IPv4 all-interfaces (`0.0.0.0`) by default and must be kept private by deployment.

## The contract

`api/openapi/service.yaml` is generated from the `#[utoipa::path]`
annotations and schema derives in the code and committed. It is the artifact
reviewers read, Redocly lints, and oasdiff judges compatibility on; the Rust
annotations are where a change is made. The document is OpenAPI 3.1.

| Command | Does |
| --- | --- |
| `make openapi-generate` | Rewrite `api/openapi/service.yaml` from the `openapi` binary |
| `make openapi-check` | Redocly lint plus the service package's contract tests: the committed file equals the generator output, effective root/operation security is unambiguous, and problem schemas are closed |
| `make openapi-lint` | Redocly lint alone (`.redocly.yaml`); part of `make check` and CI |
| `make openapi-breaking BASE_OPENAPI=<file>` | oasdiff breaking-change comparison; CI runs it on pull requests against the base branch |

The drift test runs under `make test`, so a contract change that was not
regenerated fails everywhere tests run.

### Adding an operation

1. Decide whether the operation is public or should inherit the authentication
   profile's root bearer default. A public operation in that profile declares
   `security: []`; an inherited protected operation leaves `security` absent.
   `x-security-decision` is optional authoring context and, when present, must
   agree with the effective policy. Do not add placeholder authentication.
2. Put the behavior in its feature crate. A feature depends on no provider
   crate, and no crate a feature depends on may depend on it; its HTTP
   module may use `infra-http`'s inbound contract surfaces. The handler that
   maps requests and responses for it lives with the feature's router.
3. Write the handler with `#[utoipa::path]`: `operation_id`, `summary`,
   `tag`, each needed security override, and every
   response. Name `infra_http::problem::responses::TransportProblemResponses`
   in `responses(...)` for the shared OpenAPI `400`/`413`/`500` Problems.
   The hardened chain also emits Problem `503`/`504`/`405`; header overflow
   is hyper-native `431`, not a Problem. Reference
   an operation-specific one as `(status = <code>, response = <Name>)`, and
   add a new component to `infra_http::problem::responses` only when a new
   status appears. A response family owned by one optional pack instead
   lives with that pack's seam and is registered through the pack's own
   composer. Request and response types derive `ToSchema`;
   `#[serde(deny_unknown_fields)]` closes an object.
4. Merge the feature's `ContractRouter` in `service::api::contract()`; the
   hardened chain is not edited for an operation.
5. Run `make openapi-generate`, review the YAML diff as the contract change,
   then `make openapi-check`.
6. Test the mounted router with a one-shot call asserting status, content
   type, problem code, and headers.

The first operation that accepts parameters or a body also adds the mapping
from extractor rejections to `400`/`415`/`422` problems with `invalid_params`
(axum's defaults answer `text/plain`), constraint enforcement for `pattern`
and length keywords, and boundary tests for the happy path and each invalid
path, query, body, and unknown field. The probes cannot exercise request
validation, so none of this exists yet.

A protected operation inherits the profile's root bearer requirement unless it
needs a different accepted contract. The authentication owner supplies its
fixed failure family; an author adds only operation-specific responses.

<!-- template:begin authn:docs-http-protected-composition -->
With a retained authentication profile, feature routers register documented
handlers through `infra_http::routes!` and service merges their
`ContractRouter` carriers. Bootstrap passes the complete carrier, prepared
verifier, and effective token bound to `infra_http::authn::finalize`. The one
outer layer derives policy from the final document and actual registrations,
without changing unknown-path `404` or wrong-method `405` behavior. Its
`VerifiedPrincipal` extractor contains only a sealed verified principal;
handlers never receive a raw token or unverified claims. Health probes remain
explicitly public, so supplied Authorization does not cause provider work.
The [authentication guide](../authentication.md) owns retained-profile trust,
failure, provider-budget and lifecycle decisions.
<!-- template:end authn:docs-http-protected-composition -->
<!-- template:begin request-budget:docs-http-request-budget -->
The hardened chain stamps `infra_http::RequestDeadline` immediately before
the existing request timer. Its `at()` accessor exposes the same absolute
instant without allowing a reset. A request-owned provider must observe it,
subtract its own response reserve, and refuse a missing stamp; the outer timer
remains the final inbound timeout authority.
<!-- template:end request-budget:docs-http-request-budget -->
<!-- template:begin http-idempotency:docs-http-idempotent-composition -->
With a retained idempotency profile, compose an idempotent operation through
`Composer::route(RegisteredRoutes)`. Key handling retains the registered-route
carrier; final authentication remains outside it, so at request time
authentication decides before key validation, and both precede any handler
extractor. `Composer::agree(document)` re-checks the declaration rules
against the assembled contract and activates the boundary; a violation fails
startup and the contract tests. The [HTTP idempotency guide](../http-idempotency.md)
owns retained-profile activation, retry, and data-custody decisions.

The composed seam captures the bounded original URI, received Content-Type
values, and raw body once, restores the body for extraction, then lets normal
validation and authorization run on every replay before `execute(work)`.
Generated OpenAPI owns the required key and Problem metadata; no adopter
supplies an operation namespace, fingerprint, or manual idempotency declaration.
<!-- template:end http-idempotency:docs-http-idempotent-composition -->
<!-- template:begin jobs:docs-http-jobs-worker-listener -->
With the jobs pack retained, the `jobs-worker` process serves the same probe
routes (`GET /health/live`, `GET /health/ready`) through the same hardened
chain (`infra_http::harden` with the same options) and bounded server
(`infra_http::Server`) on a health-only listener (`http.addr`), with no API
route and no OpenAPI document; `/metrics` stays on the diagnostics listener.
The [guide](../background-jobs.md#run-and-stop-the-worker) covers running and
stopping the worker.
<!-- template:end jobs:docs-http-jobs-worker-listener -->


### Compatibility

`operationId` is a stable identifier. oasdiff fails a pull request on a
breaking change (`--fail-on ERR`); an intentional exception goes into
`api/openapi/breaking-changes-approvals.txt` as an exact, temporary entry
with its owner, migration rationale, deadline, and consumer-removal evidence
in the pull request, and is removed after the change merges. Use OpenAPI
`deprecated: true` with migration guidance and a removal owner. Git is the
distribution mechanism until a real consumer needs a bundled document or a
generated client; then publish immutable versioned artifacts.

## Decisions Recorded Here

Made in stages 2 and 3 with the research behind them; a later change reopens
one only with new evidence.

### Request path

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| Every middleware applied with `Router::layer`, `LoadShedLayer` outside `GlobalConcurrencyLimitLayer`, `HandleErrorLayer` mapping `Overloaded` → `503` + `Retry-After: 1` and `Elapsed` → `504` | `route_layer`, a plain `ConcurrencyLimitLayer` | `Router::layer` covers the `404`/`405` fallbacks; a plain `ConcurrencyLimitLayer` becomes per-route under `Router::layer` (verified in tower source) |
| No `CorsLayer` | an empty `CorsLayer` | an empty layer answers every `OPTIONS` with `200`; browser cross-origin requests are fail-closed by omission until a profile decides |
| Inbound `X-Request-ID` accepted only within `^[A-Za-z0-9._~-]{1,128}$`, otherwise replaced by a UUIDv4 | tower-http's default, which trusts any present header | a caller-provided id is data, not identity; the grammar bounds log and header size |
| Template-owned one-line access log with `Option<MatchedPath>` and route-based probe suppression | tower-http `TraceLayer` alone | route templates, not raw paths, keep label cardinality bounded; `MatchedPath` is absent in `Router::fallback`, so unmatched requests carry an explicit label |
| `413` as a `Problem` body | tower-http's short-circuit | the short-circuit answers `text/plain`; the contract promises `application/problem+json` |
| Template-owned `Problem` (`code`, `request_id`, `invalid_params`) with a closed `Code` catalog | `problem_details` 0.10 (acceptable), `problemdetails` 0.7 (pins tower-http 0.6) | about sixty lines; nothing submitted by the caller is echoed; a new code is a reviewed contract change |
| Template-owned accept loop over `hyper_util::server::conn::auto` with `TokioTimer`, a `Semaphore(max_connections)` permit per connection, and a bounded `peek` before hyper sees the socket | `axum::serve` | `axum::serve` sets no timer and exposes no limits (axum #2741); the `auto` builder starts no timer until the first byte (hyper #3756), so a silent client would hold a connection forever |
| One `http.header_read_timeout` that hyper restarts on idle, so it is both the header and the keep-alive idle bound; body reads are bounded by `http.request_timeout` because extractors run inside the handler future | Go's read/write/idle deadlines | hyper has no per-connection read/write deadlines; one value covers both risks. Streaming response bodies stay unbounded until a streaming operation adopts `ResponseBodyTimeoutLayer` |
| `header_read_timeout(Some(_))` always paired with `timer()`; `max_buf_size` at least 8192 | — | both panic at `serve_connection` otherwise |

### Contract

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| Code-first: `utoipa` 5 + `utoipa-axum` 0.2 generate `service.yaml` from the handlers; the committed file is the reviewed authority, tied by a byte-exact test | `openapi-generator` `rust-axum` (JVM) spec-first; `aide` as runner-up | no maintained Rust-native spec-first server generator exists for axum; the JVM generator's output failed on quality (handler-trait shape, validation, error mapping). Oxide's `dropshot` uses the same committed-document model at scale |
| OpenAPI 3.1 | 3.0.3 | utoipa 5 emits 3.1 only; Redocly and oasdiff handle it (verified) |
| Extractors are the request validator; `#[serde(deny_unknown_fields)]` closes objects | a runtime spec validator (`openapi3filter` in Go) | no spec-driven validator exists for axum and none is needed when the types are the source; constraint keywords (`pattern`, `minLength`) get explicit enforcement with the first constrained parameter |
| `Problem.code` rendered as `string` (`value_type = String`) although the catalog is a closed enum | an enum in the schema | oasdiff classifies a new enum value in a response as breaking, and the catalog grows with features |
| Optional members `#[schema(nullable = false)]` | utoipa's default `type: [string, 'null']` | the wire omits the member and never sends `null`; the default over-promises and oasdiff flags it |
| An authentication profile declares root bearer security; public operations override it with `security: []` | a placeholder scheme or per-handler enforcement | Missing operation security inherits the root default, while `security(())` renders `[{}]` and is ambiguous. A no-auth output has no bearer scheme or per-operation security mandate |
| `info.title` a literal; `info.version` and `license` from Cargo metadata | literals the initializer rewrites | Cargo owns the version (`app.version` uses it); the initializer rewrites the title with the other identity strings |
| Drift check as a test embedding the committed file | hashing generated files around a generate step | runs everywhere `make test` runs; `make openapi-check` names it |
| Redocly alone (`.redocly.yaml` ported unchanged) | kin-openapi validation in addition | Redocly's `struct` rule validates structure and the document is type-constructed; `vacuum` is the fallback if Node stops being available |
| oasdiff through `go run` with `breaking-changes-approvals.txt` as `--err-ignore` | the Docker-based GitHub Action | one code path locally and in CI, pinned in `tools/versions.env` |

<!-- template:begin authn:docs-http-bearer-default-exception -->
When an authentication profile is retained, its real `bearerAuth` scheme and
global bearer security default are intentionally present before a product route
exists. Public operations, including probes, explicitly override it with
`security: []`; an operation with no security field inherits bearer protection.
<!-- template:end authn:docs-http-bearer-default-exception -->

### Deferred, with the change that reopens each

- Extractor rejection to `Problem` (`400`/`415`/`422` with `invalid_params`,
  RFC 6901 pointers for body members) and constraint enforcement (`garde` or
  `validator` through `axum-valid`, or newtypes with `TryFrom`): the first
  operation with parameters or a body.
- The bearer scheme, the global security requirement, and the `401`/`403`
  problem components: the authentication profile.
- `ResponseBodyTimeoutLayer`: the first streaming operation.
- Rate limiting (`tower_governor`) and CORS: profile decisions.
- `x-extensible-enum` on `Problem.code`, Swagger UI, publishing the
  document: a consumer that needs them.

### Gotchas

<!-- template:begin inbound-webhooks:docs-http-inbound-webhooks -->
## Signed webhook ingress

When retained, `POST /webhooks/{endpoint_id}` is an annotated ContractRouter
operation. It deliberately uses the existing zero-group `security()` annotation
to generate explicit `security: []`, overriding root bearer authentication. That
OpenAPI expression means bearer is not required; it does not waive the required
Standard Webhooks header/signature verification performed before persistence.

The handler receives bounded raw bytes, maps receiver outcomes to 204/400/404/
409/413/503 problems, and preserves existing hardened-chain outcomes. The
generated OpenAPI document remains handler-derived and must not be hand-edited.
<!-- template:end inbound-webhooks:docs-http-inbound-webhooks -->

1. `routes!(a, b)` groups methods of one path; two `GET` handlers on
   different paths in one `routes!` panic with "Overlapping method route".
   One `.routes(routes!(handler))` per path.
2. `extensions(("x-security-decision" = json!({...})))` and `example =
   json!(...)` need the literal `json!` token, not `serde_json::json!`.
3. A schema referenced only through a `ToResponse` component must be listed
   in `components(schemas(...))` or its `$ref` dangles.
4. utoipa overwrites a schema silently when two types share a name
   (juhaku/utoipa#1154); use `#[schema(as = ...)]`. The drift test shows the
   result without naming the cause.
5. Start from `ContractRouter::with_openapi(ApiDoc::openapi())`; it retains
   service-level metadata while tracking each actual registered method.
6. `IntoResponses` and `ToResponse` derive documentation only; the runtime
   `IntoResponse` is hand-written and the contract tests assert both agree.
7. oasdiff treats a removed non-success status as non-breaking under
   `--fail-on ERR`; a removed success status or a required property that
   became optional is an error.
8. Inside `infra-http`, the local `health` module shadows the `health`
   crate; the crate is written `::health::ReadinessReader`.
9. utoipa takes a handler's doc comment as the operation description and a
   type's doc comment as the schema description: write them as contract
   text and keep implementation notes in `//` comments.
10. `axum-tracing-opentelemetry` spans are TRACE-level without the
    `tracing_level_info` feature; `global::set_text_map_propagator` must be
    called explicitly or every request starts a new root trace.
