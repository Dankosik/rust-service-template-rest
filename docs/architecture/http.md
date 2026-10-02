# HTTP Architecture

Load for an HTTP contract, route, middleware, exposure, or handler-composition
change. The decisions behind the request path and the contract, with the
alternatives they beat, are recorded at the end of this document.

## Request path

1. `bootstrap` in `crates/service` builds configuration, telemetry, and
   readiness, takes the route tree from `service::api::contract()`, applies
   the one `service::AppState`, wraps it in `infra_http::harden`, and binds
   it with `infra_http::Server`. A router names the part of the state it
   reads (`ReadinessReader: FromRef<S>`), so a route whose state bootstrap
   does not supply fails to compile.
2. The bounded server (`crates/infra-http/src/server.rs`) owns
   connection-level policy: the connection cap, the header timeout that
   doubles as the keep-alive idle bound, the header-size bound (`431` as
   `text/plain`, before routing), the deadline on the protocol sniff, and
   the graceful drain.
3. The hardened chain (`crates/infra-http/src/harden.rs`) owns request-level
   policy, outermost first: request-id admission and propagation, `nosniff`,
   one observation middleware (the OpenTelemetry server span, HTTP metrics,
   problem completion that fills `request_id` into every Problem, and the
   access log), the `traceparent` response header, in-flight admission
   (`503` shedding with `Retry-After`, which the probe routes bypass), error
   mapping (`504` timeout with code `request_timeout`), the request timeout,
   panic recovery (`500`), the tower-http body limit, and the extractor body
   limit (`413`). Every layer is applied with `Router::layer`, so the `404`
   and `405` fallbacks travel through the same chain. There is no CORS
   layer: browser cross-origin requests are fail-closed by omission.
4. The route tree is one `utoipa_axum::router::OpenApiRouter`.
   `infra_http::router()` contributes the platform probes and the problem
   components; feature routers merge with it in `service::api::contract()`.
   The finalizer derives policy from its assembled document, splits it into
   the served axum `Router` and OpenAPI, and applies policy to served methods.
   The document-only path renders the same composition.
5. Handlers return the typed responses their contract declares (an
   `IntoResponses` enum with one variant per status, or a `Problem`).
   Failures are `Problem` values from the closed catalog in
   `infra_http::problem`, rendered as `application/problem+json` with the
   request id; no submitted value is echoed. Request data enters through
   `infra_http::extract::{Json, Query, Path}`, whose rejections are Problems;
   Clippy refuses axum's own three in application code.
6. Edge observability uses route templates, not raw paths; unmatched requests
   carry an explicit label. `/metrics` stays on the separate diagnostics
   listener, which also answers `GET /health/live`, owned by
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
   `#[serde(deny_unknown_fields)]` closes an object. Take a body, query, or
   path parameters through `infra_http::extract::{Json, Query, Path}`, not
   axum's own extractors, whose rejections answer `text/plain`; the same
   `Json` renders a JSON response body.
4. Register the handler as `OpenApiRouter::routes(utoipa_axum::routes!(handler))`
   in the feature's `OpenApiRouter`, and merge that router in
   `service::api::contract()`; the hardened chain is unchanged.
5. Run `make openapi-generate`, review the YAML diff as the contract change,
   then `make openapi-check`.
6. Test the mounted router with a one-shot call asserting status, content
   type, problem code, and headers.

`routes!` registers exactly the annotated methods, so the served routes and
the document share one source. The final policy layer answers a sanitized
`500` for a served method the document lacks, and HEAD follows GET unless the
annotation documents HEAD. Clippy's `disallowed-methods` (`clippy.toml`)
rejects the remaining escape hatches in application code: raw
`OpenApiRouter`/`Router` routes and services, fallbacks, and separately
registered HEAD or any-method handlers. Multi-handler macro calls group only
annotated methods on the same path. Its `disallowed-types` rejects
`axum::Json`, `axum::extract::Query`, and `axum::extract::Path`, so a handler
cannot take a `text/plain` rejection by importing the upstream name.

`infra_http::extract::{Json, Query, Path}` map extractor rejections into the
catalog: a body without the JSON media type is `415`
(`unsupported_media_type`), malformed JSON `400`, a body that parses but does
not fit its type `422` with the member's RFC 6901 pointer in
`invalid_params`, an oversize body `413`, and a query or path parameter that
does not fit `400` naming it as `query.<name>` or `path.<name>`. The reason
is fixed text; serde's own message quotes the submitted value and is dropped.
The first operation that accepts parameters or a body adds what no template
operation needs yet: the `415`/`422` response components in
`infra_http::problem::responses`, constraint enforcement for `pattern` and
length keywords, and boundary tests for the happy path and each invalid
path, query, body, and unknown field.

A protected operation inherits the profile's root bearer requirement unless it
needs a different accepted contract. The authentication owner supplies its
fixed failure family; an author adds only operation-specific responses.

<!-- template:begin authn:docs-http-protected-composition -->
With a retained authentication profile, feature routers register documented
handlers through `utoipa_axum::routes!` and service merges their
`OpenApiRouter` values. Bootstrap passes the complete router and prepared
verifier to `infra_http::authn::finalize`; disabled mode uses `finalize_public`.
The final document alone supplies policy. A route layer preserves native
unknown-path `404` and wrong-method `405`; explicit HEAD policy wins, otherwise
HEAD uses GET. A served method without policy produces sanitized `500`.
`VerifiedPrincipal` exposes sealed identity and immutable typed `claims<T>()`
from the same verified evidence, with sanitized access errors. Handlers never
receive raw tokens or unverified claims. The normal Clippy gate rejects raw
routes, fallbacks and separate HEAD handlers; supported authoring uses
annotated routes, OpenAPI merge and nest.
Response completeness and optional `x-security-decision` consistency belong
to the OpenAPI gate; startup checks effective security. Health probes remain
explicitly public, so supplied Authorization does not cause provider work.
The [authentication guide](../authentication.md) owns retained-profile trust,
failure, provider-budget and lifecycle decisions.
<!-- template:end authn:docs-http-protected-composition -->
<!-- template:begin request-budget:docs-http-request-budget -->
The hardened chain stamps `infra_http::RequestDeadline` immediately before
the existing request timer. Its `at()` accessor exposes the same absolute
instant without allowing a reset. Idempotency uses it for its request-owned work. Authentication has independent
provider bounds and accepts no deadline stamp or response reserve; the outer
timer alone owns `504 request_timeout`.
<!-- template:end request-budget:docs-http-request-budget -->
<!-- template:begin http-idempotency:docs-http-idempotent-composition -->
With a retained idempotency profile, compose an idempotent operation through
`Composer::route(routes)?` using the macro-only binding above. The fallible
local composition validates the route carrier before it can be served;
`Composer::finish(self) -> Activation` then activates the count of successfully
composed operations without reading the assembled document. Key handling wraps
the upstream `UtoipaMethodRouter` tuple; final authentication remains outside
it, so at request time authentication decides before key validation, and both
precede any handler extractor. The [HTTP idempotency guide](../http-idempotency.md)
owns activation, retry, and data-custody decisions.

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

<!-- template:begin inbound-webhooks:docs-http-inbound-webhooks -->
## Signed webhook ingress

When retained, `POST /webhooks/{endpoint_id}` is an annotated `OpenApiRouter`
operation. It deliberately uses the existing zero-group `security()` annotation
to generate explicit `security: []`, overriding root bearer authentication. That
OpenAPI expression means bearer is not required; it does not waive the required
Standard Webhooks header/signature verification performed before persistence.

The handler receives bounded raw bytes, maps receiver outcomes to 204/400/404/
413/503 problems, and preserves existing hardened-chain outcomes. The
generated OpenAPI document remains handler-derived and must not be hand-edited.
<!-- template:end inbound-webhooks:docs-http-inbound-webhooks -->

## Decisions Recorded Here

Made in stages 2 and 3 with the research behind them; a later change reopens
one only with new evidence.

### Request path

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| Every middleware applied with `Router::layer`; one `admit` middleware over a router-wide `Semaphore` sheds with `503` + `Retry-After: 1` and lets the probe routes through without a permit; `HandleErrorLayer` maps `Elapsed` → `504` | `route_layer`; `LoadShedLayer` over `GlobalConcurrencyLimitLayer`; a plain `ConcurrencyLimitLayer` | `Router::layer` covers the `404`/`405` fallbacks. The tower layers cannot exempt a route, so a saturated instance answered `503` to `/health/live` and the platform would restart it under load; `infra-grpc` sheds with the same `admit` shape. A plain `ConcurrencyLimitLayer` becomes per-route under `Router::layer` (verified in tower source). The connection cap still applies to a probe's connection, so the diagnostics listener, with its own cap, serves `/health/live` as well ([Runtime Lifecycle](runtime-lifecycle.md#readiness-and-liveness)) |
| No `CorsLayer` | an empty `CorsLayer` | an empty layer answers every `OPTIONS` with `200`; browser cross-origin requests are fail-closed by omission until a profile decides |
| Inbound `X-Request-ID` accepted only within `^[A-Za-z0-9._~-]{1,128}$`, otherwise replaced by a UUIDv4 | tower-http's default, which trusts any present header | a caller-provided id is data, not identity; the grammar bounds log and header size |
| Template-owned one-line access log with `Option<MatchedPath>` and route-based probe suppression | tower-http `TraceLayer` alone | route templates, not raw paths, keep label cardinality bounded; `MatchedPath` is absent in `Router::fallback`, so unmatched requests carry an explicit label |
| One observation middleware (`observe.rs`) opens the server span from `tracing-opentelemetry-instrumentation-sdk` pieces with `http.route`, `otel.name`, and `request_id` set at creation, and emits the HTTP metrics through the `metrics` facade, as `infra-grpc`'s `observe` does | `axum-tracing-opentelemetry`'s `OtelAxumLayer` plus `axum-prometheus` plus separate access-log and problem-completion layers | every `Span::record` re-serialized the span in `json-subscriber` (the JSON layer then in use), `axum-prometheus` allocates about two dozen times per request, and each `from_fn` layer clones the inner stack; on a dedicated 4-vCPU host with JSON logs and an always-on tracer this removed about 16% of the instructions and 19% of the allocations of a small request. Reopen if the upstream layer takes creation-time fields |
| Only `otel.name`, `otel.kind`, and `request_id` are `tracing` fields of the server span; the other HTTP attributes and the error status are set with `OpenTelemetrySpanExt`, and the `tracing-opentelemetry` layer adds no source location, thread, or busy/idle attributes | every HTTP attribute as a span field, flattened into each JSON log record | the JSON log layer serializes every span field and repeats it on each record inside the request, so the access line carried `url.path`, `user_agent.original`, and `server.*` twice over its own fields; exported spans keep the same HTTP attributes (`exported_server_span_keeps_the_http_attributes_and_the_error_status`). On a dedicated 4-vCPU host with JSON logs this removed about 29% of a small request's instructions. Log records keep `request_id`, trace and span ids, and the fields of other spans such as `job_attempt`; the Go template logs the same correlation set |
| HTTP metrics under the OpenTelemetry HTTP semantic-convention names as Prometheus renders them: `http_server_request_duration_seconds{http_request_method, http_route, http_response_status_code}` and `http_server_active_requests{http_request_method}`; the request count is the histogram's `_count`; an extension method is `_OTHER` and an unmatched request `http_route="<unmatched>"` | the `axum-prometheus` names (`axum_http_requests_total`, `..._duration_seconds`, `..._pending` with `method`/`endpoint`/`status`) | `axum-prometheus` left the dependency graph with the observation middleware, the server span already follows the conventions, and the template's other server counters are `http_server_*`. A separate request counter repeated the histogram's `_count`. A service that adopted the old names renames its queries: `rate(axum_http_requests_total[..])` becomes `rate(http_server_request_duration_seconds_count[..])` |
| The server span sets `http.route` and `user_agent.original` only when the request has one, `url.scheme` from an absolute-form target (HTTP/2 `:scheme`) and otherwise `http`, and `error.type` with the status code on a `5xx` | the upstream helpers' empty strings for a missing route, user agent, or scheme | the conventions omit an attribute the request lacks and require `url.scheme`; an HTTP/1 request line carries no scheme and the listener under the hardened chain is plaintext, so `http` is the scheme of the request as received. A deployment that terminates TLS in this process for HTTP/1 revisits it. `error.type` stays off the duration histogram, whose status label already separates failures |
| The span's `url.query` replaces the values of `AWSAccessKeyId`, `Signature`, `sig`, and `X-Goog-Signature` with `REDACTED` | the raw query string | the HTTP conventions' default redaction list; the rest of the query stays for diagnosis |
| tower-http `RequestBodyLimitLayer` plus axum `DefaultBodyLimit` at `http.max_body_bytes`; problem completion maps their `text/plain` `413` to the `Problem` envelope | a template-owned body-limit middleware | the stock layer already short-circuits on `Content-Length` and caps streamed bodies; only the envelope is template policy |
| Template-owned `Problem` (`code`, `request_id`, `invalid_params`) with a closed `Code` catalog | `problem_details` 0.10 (acceptable), `problemdetails` 0.7 (pins tower-http 0.6) | one serializable struct over the shared `service_failure::Code` catalog, which a general crate cannot close; nothing submitted by the caller is echoed; a new code is a reviewed contract change |
| Every problem has `type` `about:blank` and the HTTP status phrase as `title`; the `code` extension member is the one identifier | an RFC 9110 section URI per status with a title per code; a service-owned type URI per code | RFC 9457 makes `type` the primary identifier and asks one type to keep one title, but several codes shared a section URI under different titles. `about:blank` is the value the RFC defines for a problem that says no more than its status, and the template has no URI space a derived service owns. Reopen when a service publishes problem documentation at URIs it controls |
| Template-owned accept loop over `hyper_util::server::conn::auto` with `TokioTimer`, a `Semaphore(max_connections)` permit per connection, and a socket wrapper (`SniffDeadline`) that fails reads once `http.header_read_timeout` passes with the protocol still undecided | `axum::serve`; a bounded `peek` for the first byte before hyper sees the socket | `axum::serve` sets no timer and exposes no limits (axum #2741). The `auto` builder reads until the bytes stop matching the HTTP/2 preface and starts no timer before that (hyper #3756), so a client that sends nothing, or only the start of the preface, would hold a connection forever. The `peek` closed the first case and let the second through: one byte of the preface passed it. The wrapper is a pass-through once the protocol is decided, and reads the decrypted stream on a TLS listener, where a peek sees only the handshake |
| One `http.header_read_timeout` that hyper restarts on idle, so it is both the header and the keep-alive idle bound; body reads are bounded by `http.request_timeout` because extractors run inside the handler future | Go's read/write/idle deadlines | hyper has no per-connection read/write deadlines; one value covers both risks. Streaming response bodies stay unbounded until a streaming operation adopts `ResponseBodyTimeoutLayer` |
| `TCP_NODELAY` on every accepted socket | Nagle's algorithm, the kernel default | hyper sends HTTP/2 headers, data and trailers as separate segments, so a later one waited for the peer's delayed ACK: gRPC unary calls with 1 KiB messages and client-streaming calls stalled 41 ms each (1.5k → 19.8k calls/s on DigitalOcean c-4). Tonic's own server and grpc-go set it too. Saturated tiny-message streams lose 5–9% throughput to the extra packets |
| Each connection runs in a `tokio_util` `TaskTracker` task that selects over the connection, the drain token, and `max_connection_age`, then calls hyper's `graceful_shutdown` and waits for the connection to end. A TLS handshake still running at drain is dropped, since no request can be in flight before it ends. The age is spread by up to 10% either way. `http.max_connection_age` defaults to off; `grpc.max_connection_age` to thirty minutes | `hyper_util::server::graceful::GracefulShutdown`, which signals only at drain and whose connection trait is sealed, so nothing else can ask one watched connection to finish; tonic's own `Server`, which has `max_connection_age` but is a second accept loop | A long-lived HTTP/2 connection behind a connection-level balancer keeps every call on the replica it first reached, so replicas added later get none until something closes it. grpc-go (`MaxConnectionAge`, with the same spread), Envoy (`max_connection_duration`) and nginx (`keepalive_time`) bound a connection's age for this reason. This is hyper's documented graceful-shutdown shape and what tonic's serve loop does. HTTP stays off because an HTTP/1 proxy that reuses an idle connection just as the server closes it sees a failed request; HTTP/2 GOAWAY has no such race. There is no forced close after the age: a stream that outlives it keeps its connection |
| `header_read_timeout(Some(_))` always paired with `timer()`; `max_buf_size` at least 8192 | — | both panic at `serve_connection` otherwise |

### Contract

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| Code-first: `utoipa` 6 + `utoipa-axum` 0.3 generate `service.yaml` from the handlers; the committed file is the reviewed authority, tied by a byte-exact test | `openapi-generator` `rust-axum` (JVM) spec-first; `aide` as runner-up | no maintained Rust-native spec-first server generator exists for axum; the JVM generator's output failed on quality (handler-trait shape, validation, error mapping). Oxide's `dropshot` uses the same committed-document model at scale |
| OpenAPI 3.1 | 3.0.3; 3.2 | utoipa 6 emits 3.1 by default and never 3.0; Redocly and oasdiff handle 3.1 (verified). 3.2 is opt-in in utoipa 6 and waits for a feature the contract needs |
| Extractors are the request validator; `#[serde(deny_unknown_fields)]` closes objects | a runtime spec validator (`openapi3filter` in Go) | no spec-driven validator exists for axum and none is needed when the types are the source; constraint keywords (`pattern`, `minLength`) get explicit enforcement with the first constrained parameter |
| `infra_http::extract::{Json, Query, Path}` wrap axum's extractors and turn each rejection into a `Problem`; the body pointer comes from the `serde_path_to_error` path axum already records (direct dependency on 0.1.20, the version axum resolves) | axum's extractors with their `text/plain` rejections; `axum-extra`'s `WithRejection`; `#[derive(FromRequest)]` with `rejection(...)`; a response-rewriting layer | the wrappers are three short `FromRequest` impls. `clippy.toml` `disallowed-types` rejects axum's three in application code, and `Json` also implements `IntoResponse` so the ban leaves one name for both directions. `WithRejection` needs a second type parameter at every handler, the derive adds the `axum-macros` proc-macro crate for the same three impls, and rewriting bare `4xx` responses cannot tell a rejection from a handler's own answer. A body member is named by its location, which is the caller's own key, so a location over 256 bytes is omitted; the submitted value never appears |
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

- The `415`/`422` response components and constraint enforcement (`garde` or
  `validator` through `axum-valid`, or newtypes with `TryFrom`): the first
  operation with parameters or a body.
- The bearer scheme, the global security requirement, and the `401`/`403`
  problem components: the authentication profile.
- `ResponseBodyTimeoutLayer`: the first streaming operation.
- Rate limiting (`tower_governor`) and CORS: profile decisions.
- Response compression (tower-http `CompressionLayer`): the first operation
  whose bodies are large enough to pay for it and whose platform edge does not
  compress; it joins the feature router, not the chain, and never wraps a
  response that mixes a secret with caller-controlled text.
- An idle bound for HTTP/2 connections: hyper has none, and the PING
  keep-alive only closes a peer that stopped answering, so an idle HTTP/2
  connection keeps its `http.max_connections` slot until its peer closes it
  or `http.max_connection_age` is set. grpc-go's `MaxConnectionIdle` is
  unlimited by default for the same reason: a client pool keeps idle
  connections on purpose. Reopen when a listener is reachable by callers the
  deployment does not control and the connection cap is observed full of
  idle connections; the bound then counts open streams per connection.
- Problem rejections for other extractors (`Form`, `Multipart`, typed
  headers, `axum-extra`'s repeated-key `Query`): the first operation that
  takes one adds its wrapper to `infra_http::extract` and its entry to
  `clippy.toml`. Until then such an extractor answers axum's `text/plain`.
- `client.address` and `network.peer.address` on the server span: the peer
  behind a load balancer is the balancer, and a forwarded address is
  caller-controlled until a deployment names the proxies it trusts. Both are
  personal data in traces. A service that needs them decides the trusted-proxy
  rule and the retention first.
- `x-extensible-enum` on `Problem.code`, Swagger UI, publishing the
  document: a consumer that needs them.

### Gotchas

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
5. Start from `OpenApiRouter::with_openapi(ApiDoc::openapi())`; it retains
   service-level metadata while annotations supply served operations.
6. `IntoResponses` and `ToResponse` derive documentation only; the runtime
   `IntoResponse` is hand-written and the contract tests assert both agree.
7. oasdiff treats a removed non-success status as non-breaking under
   `--fail-on ERR`; a removed success status or a required property that
   became optional is an error.
8. utoipa takes a handler's doc comment as the operation description and a
   type's doc comment as the schema description: write them as contract
   text and keep implementation notes in `//` comments.
9. `tracing-opentelemetry-instrumentation-sdk` spans are TRACE-level without
   the `tracing_level_info` feature; `global::set_text_map_propagator` must be
   called explicitly or every request starts a new root trace.
10. utoipa 6 types a parameter and a response header as `RefOr<_>`: code that
    reads or builds the document matches `RefOr::T` and decides what a `$ref`
    means for its check.
