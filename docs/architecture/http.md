# HTTP Architecture

Load for an HTTP contract, route, middleware, exposure, or handler-composition
change. Decisions and their evidence are recorded in
[`specs/api-contract/research/synthesis.md`](../../specs/api-contract/research/synthesis.md)
and [`specs/runtime-core/research/synthesis.md`](../../specs/runtime-core/research/synthesis.md).

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
   (`503` shedding with `Retry-After`, `504` timeout), load shedding, the
   in-flight limit, the request timeout, panic recovery (`500`), and the body
   limit (`413`). Every layer is applied with `Router::layer`, so the `404`
   and `405` fallbacks travel through the same chain. There is no CORS
   layer: browser cross-origin requests are fail-closed by omission.
4. The route tree is one `utoipa_axum::router::OpenApiRouter`.
   `infra_http::router()` contributes the platform probes and the problem
   components; feature routers merge with it in `service::api::contract()`.
   Splitting that value yields the served axum `Router` and the OpenAPI
   document, so the two cannot describe different services.
5. Handlers return the typed responses their contract declares (an
   `IntoResponses` enum with one variant per status, or a `Problem`).
   Failures are `Problem` values from the closed catalog in
   `infra_http::problem`, rendered as `application/problem+json` with the
   request id; nothing submitted by the caller is echoed.
6. Edge observability uses route templates, not raw paths; unmatched requests
   carry an explicit label. `/metrics` stays on the separate diagnostics
   listener owned by
   [Configuration Source Policy](../configuration-source-policy.md#opentelemetry-environment-policy),
   which binds every interface and must be kept private by deployment.

## The contract

`api/openapi/service.yaml` is generated from the `#[utoipa::path]`
annotations and schema derives in the code and committed. It is the artifact
reviewers read, Redocly lints, and oasdiff judges compatibility on; the Rust
annotations are where a change is made. The document is OpenAPI 3.1.

| Command | Does |
| --- | --- |
| `make openapi-generate` | Rewrite `api/openapi/service.yaml` from the `openapi` binary |
| `make openapi-check` | Redocly lint plus the `service` contract tests: the committed file equals the generator output, every operation declares `x-security-decision` and `security`, protected operations declare their problem responses, problem schemas are closed |
| `make openapi-lint` | Redocly lint alone (`.redocly.yaml`); part of `make check` and CI |
| `make openapi-breaking BASE_OPENAPI=<file>` | oasdiff breaking-change comparison; CI runs it on pull requests against the base branch |

The drift test runs under `make test`, so a contract change that was not
regenerated fails everywhere tests run.

### Adding an operation

1. Decide the exposure first and record it as the operation's
   `x-security-decision`: `public` by design, `protected` by a real security
   scheme, or `blocked` pending a security specification. Do not add
   placeholder authentication.
2. Put the behavior in its feature crate, which depends on no transport
   crate. The handler that maps requests and responses for it lives with the
   feature's router.
3. Write the handler with `#[utoipa::path]`: `operation_id`, `summary`,
   `tag`, the extension, `security()` for a public operation, and every
   response. Reference the shared problem responses
   (`(status = 400, response = BadRequest)`) and add a new one to
   `infra_http::problem` only when a new status appears. Request and response
   types derive `ToSchema`; `#[serde(deny_unknown_fields)]` closes an object.
4. Merge the feature's `OpenApiRouter` in `service::api::contract()`; the
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

A protected operation declares a real OpenAPI security requirement (the
bearer scheme arrives with the authentication profile), `401` and `403`
problem responses, and the `400`, `431`, `503`, and `504` problem responses
the contract test requires.

### Compatibility

`operationId` is a stable identifier. oasdiff fails a pull request on a
breaking change (`--fail-on ERR`); an intentional exception goes into
`api/openapi/breaking-changes-approvals.txt` as an exact, temporary entry
with its owner, migration rationale, deadline, and consumer-removal evidence
in the pull request, and is removed after the change merges. Use OpenAPI
`deprecated: true` with migration guidance and a removal owner. Git is the
distribution mechanism until a real consumer needs a bundled document or a
generated client; then publish immutable versioned artifacts.
