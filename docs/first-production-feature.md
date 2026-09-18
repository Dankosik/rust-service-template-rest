# First production feature

The maintained path from the health-only scaffold to one useful vertical
slice. Every step below was followed on the scaffold with a `greeting`
feature (`GET /greetings/{name}` answering JSON, `404` for a reserved name)
and ended in a passing `ALLOW_FULL=1 make check`; the snippets are that
walkthrough's code.

## 1. Close the behavior and trust decision

Write down the resource, the operation, the success response, the expected
failures, and the compatibility requirement before touching code. Decide the
operation's exposure and record it as its `x-security-decision`: `public` by
design, `protected` by a real identity and authorization design, or `blocked`
pending a security specification. The template supplies no placeholder
authentication and the contract test refuses an operation without a
decision.

If the feature calls another service or persists data, also decide who owns
the source of truth, the timeout and retry eligibility inside
`http.request_timeout`, the transaction boundary, readiness participation,
and cleanup on partial startup ([Integration Boundaries](architecture/integration.md)).

## 2. Create the feature crate

A feature is one crate under `crates/<feature>`; the compiler enforces that
nothing under `crates/infra-*` depends on it. The business rule lives in
`src/lib.rs` with no transport types; the HTTP operations live in
`src/http.rs`, which may use `axum`, `utoipa`, and `infra-http`'s problem
catalog but never the chain or the server.

```toml
# crates/greeting/Cargo.toml
[package]
name = "greeting"
description = "Greeting feature: the business rule and its HTTP operations."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
publish.workspace = true

[lints]
workspace = true

[dependencies]
axum = { workspace = true, features = ["json"] }
infra-http = { workspace = true }
serde = { workspace = true, features = ["std", "derive"] }
thiserror = { workspace = true }
utoipa = { workspace = true, features = ["macros"] }
utoipa-axum = { workspace = true }

[dev-dependencies]
http-body-util = { workspace = true }
serde_json = { workspace = true, features = ["std"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
tower = { workspace = true, features = ["util"] }
```

Every dependency is declared once in the workspace table with default
features off; the crate enables what it uses. Add the crate to the workspace
table too (`greeting = { path = "crates/greeting" }` under the workspace
crates in `Cargo.toml`).

```rust
// crates/greeting/src/lib.rs
pub mod http;

const RESERVED: [&str; 2] = ["root", "admin"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Greeting {
    pub message: String,
}

/// Why a greeting was refused; each variant maps to one problem code in `http`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GreetingError {
    #[error("no greeting exists for a reserved name")]
    Reserved,
}

pub fn greet(name: &str) -> Result<Greeting, GreetingError> {
    if RESERVED.contains(&name) {
        return Err(GreetingError::Reserved);
    }
    Ok(Greeting { message: format!("hello, {name}") })
}
```

Start with sibling unit tests (`#[cfg(test)] mod tests`) for the invariants
and the error identities. Keep HTTP status codes, schemas, SQL rows, and
provider payloads out of this file. Add a trait only when the feature needs
dependency inversion over persistence or an outbound system.

## 3. Write the operation as its contract

The `#[utoipa::path]` attribute is the contract; the committed document is
generated from it ([HTTP Architecture](architecture/http.md#adding-an-operation)).

```rust
// crates/greeting/src/http.rs (the operation; imports and the router follow)
const GREETING_PATH: &str = "/greetings/{name}";

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GreetingBody {
    #[schema(example = "hello, ada")]
    pub message: String,
}

#[derive(Debug, ToResponse)]
#[response(description = "No greeting exists for this name", content_type = "application/problem+json")]
pub struct GreetingNotFound(pub Problem);

#[derive(Debug, IntoResponses)]
pub enum GreetingResponse {
    #[response(status = 200, content_type = "application/json")]
    Ok(#[to_schema] GreetingBody),
    #[response(status = 404)]
    NotFound(#[to_response] GreetingNotFound),
}

impl IntoResponse for GreetingResponse {
    fn into_response(self) -> Response {
        match self {
            Self::Ok(body) => (StatusCode::OK, Json(body)).into_response(),
            Self::NotFound(GreetingNotFound(problem)) => problem.into_response(),
        }
    }
}

#[utoipa::path(
    get,
    path = GREETING_PATH,
    tag = "greetings",
    operation_id = "getGreeting",
    summary = "Greet a name",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "the greeting carries no caller data beyond the path segment it echoes"
    }))),
    params(("name" = String, Path, description = "Who to greet")),
    responses(GreetingResponse, TransportProblemResponses)
)]
async fn get_greeting(Path(name): Path<String>, parts: Parts) -> GreetingResponse {
    match greet(&name) {
        Ok(greeting) => GreetingResponse::Ok(GreetingBody { message: greeting.message }),
        Err(GreetingError::Reserved) => GreetingResponse::NotFound(GreetingNotFound(
            Problem::new(Code::NotFound)
                .detail("no greeting exists for a reserved name")
                .request_id(infra_http::request_id(&parts.extensions)),
        )),
    }
}

#[must_use]
pub fn router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    OpenApiRouter::new().routes(routes!(get_greeting))
}
```

What each choice buys:

- `operation_id` is the stable identifier oasdiff tracks; `security()`
  renders `security: []`, the explicit public override the linter and the
  contract test require; the extension is the recorded decision from step 1.
- `TransportProblemResponses` declares the `400`, `413`, and `500` problems
  the hardened chain can answer on any route; the operation adds only its own
  statuses. A new status that several operations share becomes a
  `ToResponse` component in `infra_http::problem::responses`.
- `IntoResponses` documents the statuses; the hand-written `IntoResponse`
  serves them, and the contract tests assert both agree. `Json` gives the
  declared `application/json`; a `Problem` renders `application/problem+json`
  with the closed `code` from the catalog. `Code` is a closed enum: a code
  the catalog lacks is a reviewed contract change, not a string.
- `Parts` gives the handler the request extensions, and
  `infra_http::request_id` reads the id the correlation layer admitted, so a
  handler-produced problem carries the same `request_id` as the log line.
- `#[serde(deny_unknown_fields)]` renders `additionalProperties: false`.
- The router is generic over the state so the composition root can merge it
  beside the probe router, whose state is the readiness reader. One
  `.routes(routes!(handler))` per path.

A doc comment on the handler becomes the operation description and a doc
comment on a type becomes its schema description; write them as contract
text.

Test the operation as a caller sees it, with `tower::ServiceExt::oneshot`
against `router::<()>().split_for_parts().0`: status, `Content-Type`, body,
and for the failure the problem `code`. The walkthrough's two tests are
`greets_with_json` and `reserved_name_is_a_not_found_problem`.

## 4. Merge the router in the composition root

`crates/service/src/api.rs` owns the one route tree. Add the crate to
`crates/service/Cargo.toml` (`greeting = { workspace = true }`), a tag for
the document, and one `.merge`:

```rust
#[openapi(
    // …
    tags(
        (name = "system", description = "Operational endpoints for liveness and readiness."),
        (name = "greetings", description = "Greetings for a name.")
    )
)]
struct ApiDoc;

pub fn contract() -> OpenApiRouter<ReadinessReader> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(infra_http::router())
        .merge(greeting::http::router())
}
```

Nothing else in `service` changes: the hardened chain, the listeners, and
the teardown are unaware of the feature. A forgotten merge is loud: the
operation is not served and not documented, and the feature's own router
test still passes, so the drift test in step 5 is what proves the merge.

## 5. Regenerate and review the contract

```bash
cargo update --workspace     # the new crate joins Cargo.lock; commit it
make openapi-generate
git diff api/openapi/service.yaml
```

Review the YAML diff as the contract change: the path, `operationId`,
parameters, each response with its media type, `security: []`, and the
decision extension. The document is OpenAPI 3.1; `Problem` and the shared
responses are referenced, not repeated.

## 6. Add configuration only when the feature needs a knob

For each runtime key: the typed field, its default in `impl Default`, and
its validation, all in the section's own file under `crates/config/src/`,
plus a loader test and a rejected-value test
([Configuration Source Policy](configuration-source-policy.md#adding-a-config-key)).
Secrets are `SecretString` fields fed only by `APP__*` variables. Unknown
keys already fail startup, so a misspelled deployment variable cannot fall
back to a default silently. The greeting needed no key.

## 7. Prove it

```bash
make build
make test-changed PKGS="greeting service"   # what scripts/ci/affected-crates.sh selects
make openapi-check
ALLOW_FULL=1 make check
```

`make plan` shows the same selection for the whole worktree. The full gate on
the walkthrough ran format, clippy at pedantic, 84 tests (the four new ones
included), cargo-shear, Redocly, the skills check, the link check, and the
validation-system self-tests. Two things the gate caught on the way are worth
knowing: rustfmt reflows long attribute lines (`make fmt`), and clippy
rejects a redundant closure in a test (`Collected::to_bytes` as a path).

Then run it: `make run`, `curl -i localhost:8080/greetings/ada` answers
`200 application/json`, `curl -i localhost:8080/greetings/root` answers
`404 application/problem+json` with `"code":"not_found"` and the request id;
both carry `x-content-type-options: nosniff` from the chain.

## 8. Before production

Replace the unresolved entries in [Production Contract](production-contract.md)
with service-owned scope, dependency, capacity, durability, trust, SLO, and
recovery decisions; keep promotion blocked until their owner supplies them.
Prometheus exposition stays on the separate diagnostics listener, which
binds every interface and must be kept private by deployment. Add
low-cardinality feature metrics or spans only where they answer an
operational question ([Runtime Lifecycle](architecture/runtime-lifecycle.md),
[Railway Deployment Profile](railway-deployment-profile.md)).

## What the walkthrough did not exercise

- A request body or a constrained parameter. The first one adds the mapping
  from extractor rejections to `400`/`415`/`422` problems with
  `invalid_params` (axum's defaults answer `text/plain`) and constraint
  enforcement; [HTTP Architecture](architecture/http.md#deferred-with-the-change-that-reopens-each)
  records the options.
- A protected operation: the bearer scheme, `401`/`403`, and the
  authentication profile's bootstrap wiring.
- Persistence, an outbound dependency, or a background task: their stages
  add the adapter crate, the readiness probe, the shutdown stage, and the
  container-backed proof.
