# Stage 3 research synthesis: API contract

Decisions for roadmap stage 3 (OpenAPI contract, generated bindings, drift
check, lint, breaking-change comparison). Versions were read from crates.io
and GitHub on 2026-09-17/18. Claims marked *verified* were executed against
Rust 1.98.1 and axum 0.8.9 in a scratch project; claims marked *observed*
were read in source or generated output without being executed.

The Go template supplies the problems and the reasons behind its choices:
`api/openapi/service.yaml` as the reviewed contract, `oapi-codegen` strict
server with a runtime request validator, `x-security-decision` on every
operation, Redocly lint, kin-openapi validation, oasdiff against the
pull-request base, and a contract test that iterates the spec. Where Rust
solves a problem differently, the Rust way wins and the deviation is recorded
in the last sections.

## Requirements

From the roadmap and the Go reference (`internal/infra/http/router.go`,
`openapi_contract_test.go`, `internal/openapi/README.md`):

- typed request extraction and typed responses per status;
- the committed spec is the artifact reviewers read and compatibility is
  judged on; regenerating from it produces no diff; a deliberate change is
  caught by `make openapi-check` and CI; nothing generated is hand-edited;
- lint and structural validation of the spec; breaking-change comparison
  against the pull-request base with an explicit approvals file;
- integration with the hardened chain: problems from the closed
  `infra_http::problem` catalog, `request_id`, `application/problem+json`;
- runtime contract tests: every operation declares its security decision;
  a protected operation declares the required problem responses;
- health probes are served through the contract-bound interface.

## Candidates

| Candidate | Latest (date) | Maintenance signal | Direction | axum 0.8 | Verdict |
| --- | --- | --- | --- | --- | --- |
| `openapi-generator` `rust-axum` | 7.25.0 (2026-08-24) | active; JVM (Docker image available) | spec → trait + router + models | yes: templates pin axum 0.8, axum-extra 0.12, validator 0.20, edition 2024 | rejected on generated-output quality (below) |
| `utoipa` + `utoipa-axum` | 5.5.0 (2026-05-04) + 0.2.0 (2025-01-16) | repository pushed 2026-09-16, 42 commits since 5.5.0; 46.8 M downloads | code → document | yes: 0.2.0 targets axum ≥ 0.8.4 | **selected** |
| `aide` | 0.15.1 (2025-08-19); 0.16.0-alpha.4 (2026-04-14) | pushed 2026-04-14, 36 commits since 0.15.1; 2.8 M downloads | code → document over schemars 0.9 | yes | runner-up |
| `poem-openapi` | 5.1.16 (2025-07-28) | active | code → document | no: poem framework | rejected |
| `oasgen` | 0.25.0 (2025-02-25) | pushed 2025-08-22 | code → document | yes | rejected: stale |
| `dropshot` | 0.17.1 (2026-06-02) | active | API trait → document; committed spec checked in CI | no: own framework | not a candidate; precedent for the selected model |
| `progenitor` / `typify` | 0.15.0 / 0.8.0 (2026-09) | active | spec → client / JSON Schema → types | n/a | not server generators; `typify` stays the option for *consuming* external contracts |
| `paperclip` | 0.9.7 (2026-04-20) | slow | actix-web plugin and client codegen | no | rejected |
| `swagger` (`rust-server` runtime) | 7.0.1 | legacy | hyper 0.14 runtime for the old generator | no | rejected |
| `openapiv3` / `oas3` | 2.2.0 (2025-06-02) / 0.22.0 (2026-05-06) | `openapiv3` is 3.0-only and quiet since 2025-06; `oas3` reads 3.1 and is active | parsers | — | not needed: drift compares bytes and the contract tests read the in-memory document |

Tooling around the committed file:

| Tool | Version | Role | Verdict |
| --- | --- | --- | --- |
| Redocly CLI | 2.53.3 (2026-09-17) | lint plus structural validation (`struct` rule inside `recommended`); the Go `.redocly.yaml` ported verbatim passed on the generated 3.1 document (*verified*) | selected, via `npx --yes @redocly/cli@<pinned>` as in the Go template |
| `vacuum` | 0.30.6 (2026-09-15) | Spectral-compatible single Go binary | alternative if Node becomes a burden; the Redocly ruleset would need rewriting |
| `oasdiff` | 1.32.1 (2026-09-15) | breaking-change comparison; reads 3.1 documents (*verified*: it interpreted `type: [string, 'null']` as nullable); GitHub Action v0.1.17 is Docker-based | selected |
| Spectral | 6.16.3 | lint | not selected: no ruleset to port |

## Why not spec-first generation

There is no maintained Rust-native generator that turns an OpenAPI document
into axum server bindings. Every Rust OpenAPI server stack (`utoipa`, `aide`,
`poem-openapi`, `dropshot`, `oasgen`, `apistos`, `salvo-oapi`) derives the
document from code; `progenitor` and `typify` generate clients and types. The
only spec-first server generator is the JVM `openapi-generator` `rust-axum`
target. Generating the health-only contract with it (7.25.0, *observed*):

- regeneration needs a JVM or the Docker image;
- 1,859 lines and `ammonia` (an HTML parser for "XSS" string checks),
  `chrono`, `uuid`, `regex`, `lazy_static`, `base64`, `validator`,
  `axum-extra` and axum's `multipart` feature for two probe operations;
- every generated handler extracts `TypedHeader<Host>` and `CookieJar` and
  the trait takes `&Method, &Host, &CookieJar` on every operation: a request
  without a `Host` header is refused with a bare `400` before the operation
  runs, and hyper's HTTP/2 server path carries the authority in the URI and
  inserts no `Host` header (`proto/h2/server.rs` at 1.11.1 has no host
  handling);
- parameter validation and JSON serialisation run inside `spawn_blocking`
  with `.unwrap()` on the join handle;
- failures are bare `StatusCode` or text bodies (`validation_error.to_string()`
  as the `400` body), bypassing the problem catalog;
- `models::Problem` is a second copy of `infra_http::Problem` carrying
  `validator` derives, the very drift the one-catalog rule exists to prevent;
- vendor extensions are dropped from generated output (harmless).

Any of these can be changed only through custom mustache templates, which is
template-owned generator code to maintain. Rejected.

## Decision: code-first with the committed spec as the reviewed authority

The Rust types and the `#[utoipa::path]` annotations on the handlers are the
generation source. `api/openapi/service.yaml` is generated from them,
committed, and is the artifact reviewers read, Redocly lints, and oasdiff
judges compatibility on. A byte-exact test ties the two: code that changes
the contract without regenerating fails `make test`, `make openapi-check`,
and CI. This is the model the roadmap allows ("code-first is acceptable only
if the committed spec remains the reviewed authority") and the model
Oxide's `dropshot` uses at scale.

| Area | Decision | Crates (version) | Template-owned code (named gaps) |
| --- | --- | --- | --- |
| Route tree | `utoipa_axum::router::OpenApiRouter` with one `.routes(routes!(handler))` per path; `split_for_parts()` yields the axum `Router` that `infra_http::harden` wraps and the `OpenApi` document; the served router and the document come from one construction (*verified*) | `utoipa-axum` 0.2.0 | — |
| Document identity | `#[derive(OpenApi)]` in `crates/service`, the composition root: `info.title` and `info.description` as literals (the repository name; the crate description is about the binary, not the API), `version` and `license` filled from the service crate's Cargo metadata (utoipa's `Info::merge_with_env_args`, rendering the 3.1 `identifier: MIT` form), `servers`, `tags`; `infra-http` contributes the probe operations and problem components through `OpenApiRouter::merge`, which leaves `info` untouched (*verified* in source); feature crates will contribute the same way | `utoipa` 5.5.0 (`macros`, `yaml`) | the document assembly function (~20 lines) |
| Typed responses per status | one `#[derive(IntoResponses)]` enum per operation with more than one success/failure shape, plus a hand-written `IntoResponse` (utoipa derives documentation only); reusable problem responses as `#[derive(ToResponse)]` newtypes over `Problem` with `content_type = "application/problem+json"`, referenced as `(status = 400, response = BadRequest)` (*verified*) | `utoipa` | the `IntoResponse` impls, a few lines each |
| Problem schema | `ToSchema` on `Problem` and `InvalidParam`; `#[serde(deny_unknown_fields)]` renders `additionalProperties: false`; `#[serde(skip)]` and `rename` are honoured; `code` rendered as `string` through `#[schema(value_type = String)]`; optional members `#[schema(nullable = false)]` (*verified*, see deviations) | `utoipa` | — |
| Typed request extraction | axum extractors on types that also derive `ToSchema`/`IntoParams`; the health-only contract has no parameters or bodies, so nothing is added now | `axum` | first parameterized operation: extractor rejection → `Problem` mapping and constraint enforcement (deferred, below) |
| Generation | `crates/service/src/bin/openapi.rs` prints the rendered document (a one-line generated-file header, then `to_yaml()`); `make openapi-generate` redirects it into `api/openapi/service.yaml` | `utoipa` `yaml` feature (`serde_norway` 0.9.42) | ~10 lines |
| Drift check | a `crates/service` test embeds the committed file with `include_str!` and asserts byte equality with the rendered document; cargo rebuilds the test when the file changes; `make openapi-check` runs it by name and `make test` covers it | — | ~10 lines |
| Lint and validation | Redocly CLI pinned in `make/template.mk` (`REDOCLY_CLI_VERSION := 2.53.3`, run through `npx --yes`), `.redocly.yaml` ported from the Go template unchanged, `make openapi-lint`, part of `make openapi-check`, `make check`, and CI. The ported `security-defined: error` rule requires every operation to declare `security` explicitly, which is why public operations render `security: []` (*verified*: the first render without it failed lint) | — | `.redocly.yaml`, make target |
| Breaking-change comparison | `oasdiff breaking --fail-on ERR <base> api/openapi/service.yaml` with `api/openapi/breaking-changes-approvals.txt` as `--err-ignore` when the file is non-empty; `make openapi-breaking BASE_OPENAPI=...` runs `go run github.com/oasdiff/oasdiff@v1.32.1`, one code path locally and in CI, pinned in one place and integrity-checked by the Go module checksum database; CI on pull requests extracts `git show BASE:api/openapi/service.yaml` and skips when the base has no file (the first pull request). The generated document against the Go template's health-only 3.0.3 document reports no breaking change (*verified*) | — | make target, CI step |
| Runtime contract tests | iterate the in-memory `OpenApi` (paths → operations): every operation carries `x-security-decision` with `exposure` in {`public`, `protected`, `blocked`} and a `rationale`; `public` ⇔ the effective security requirements (operation, else document) are empty; `protected` ⇔ every alternative is exactly one `http`/`bearer` scheme without scopes and `400`, `401`, `403`, `431`, `503`, `504` declare `application/problem+json` referencing `#/components/schemas/Problem`; `Problem` and `InvalidParam` are closed objects | — | port of the Go test (~100 lines) |
| Serving | the probe handlers are the annotated functions; tests drive the hardened router and assert status, `Content-Type`, and body against what the document declares | `tower::ServiceExt::oneshot` | — |

`openapi.gen.go` has no Rust counterpart: nothing generated lands in Rust
source, so there is no generated file to protect from edits and no
"operations declared versus implemented" count. A route without a handler
does not compile, and the document is derived from the routes.

## Version set and resolved tree

`utoipa` 5.5.0 pulls `indexmap` 2.14, `serde_norway` 0.9.42, and the
`utoipa-gen` proc-macro; `utoipa-axum` 0.2.0 pulls `paste` 1.0.15
(proc-macro). No second `axum`, `http`, `tower`, or `serde_json` version
appears (*verified* with `cargo tree`). `utoipa` with `default-features =
false` loses `macros`; enable `macros` and `yaml` explicitly. The
`axum_extras` feature (automatic `Path`/`Query` parameter inference) is
enabled with the first parameterized operation, not before.

`paste` is flagged unmaintained (RUSTSEC-2024-0436, informational); the
unreleased `utoipa-axum` switches to `pastey`. The stage 4 `cargo-deny`
policy either ignores that advisory with this reason or the workspace moves
to the next `utoipa-axum` release, whichever comes first.

`serde_norway` is the maintained fork of `serde_yaml` 0.9 that utoipa chose
for its `yaml` feature. The stage 2 rejection of YAML concerned parsing
configuration files with a general-purpose YAML crate; here the only consumer
is the generator, the document is validated by Redocly afterwards, and the
crate is compiled into the `service` library but not linked into the runtime
binary, which never calls `to_yaml()`.

## Deviations from the Go template

| Go template | Rust template | Why |
| --- | --- | --- |
| Spec-first: `service.yaml` written by hand, `oapi-codegen` generates `openapi.gen.go` | Code-first: Rust types and annotations generate `service.yaml`; the committed file is the reviewed and compatibility-checked artifact; a byte-exact drift test ties them | No maintained Rust-native spec-first server generator exists for axum; the JVM generator's output has the defects listed above. Every Rust OpenAPI server stack works this way |
| OpenAPI 3.0.3 | OpenAPI 3.1.0 | utoipa 5 emits 3.1 only (`OpenApiVersion` has one variant); Redocly and oasdiff handle 3.1 (*verified*) |
| Runtime request validator (`openapi3filter` through `nethttp-middleware`) enforcing the embedded spec | None. Extractors are the validator: typed extraction and `#[serde(deny_unknown_fields)]` fail before the handler runs, and the document is derived from the same types, so there is no second description to enforce at runtime | No spec-driven validator exists for axum, and one is not needed when the types are the source. Constraint keywords (`pattern`, `minLength`) need explicit enforcement when the first constrained parameter arrives (deferred) |
| Embedded spec served to the validator (`openapi.GetSpec()`) | Not embedded at runtime | No runtime consumer |
| `Problem.code` documented as a free `string` while the Go catalog is closed | Same on the wire: the closed `Code` enum renders as `string` through `value_type = String` | oasdiff classifies a new enum value in a response as breaking (`response-property-enum-value-added`, *verified*), and the catalog is meant to grow with features |
| Optional members `detail`, `instance`, `request_id` are plain strings | Same, declared with `#[schema(nullable = false)]` | utoipa renders `Option<T>` as `type: [string, 'null']` by default; the wire omits the member and never sends `null`, so the default would over-promise nullability and oasdiff would flag it (*verified*) |
| Public operations carry `security: []` behind the `authn-bearer` marker; global `security: [{bearerAuth: []}]` | Public operations carry `security: []` through an empty `security()` attribute (*verified*); no global requirement while no scheme exists | Same wire form. The marker is not needed: the attribute is unconditional and the global requirement arrives with the scheme |
| `info.title` and `info.version` are literals the initializer rewrites | `info.title` a literal, `info.version` and `info.license` from the service crate's Cargo metadata | Cargo already owns the version (`app.version` uses it); the stage 9 initializer rewrites the title with the other identity strings |
| `example: ok` inside the `text/plain` schema | `example` on the media type | Where utoipa puts a response example; both forms are valid |
| Bearer security scheme present in `components` behind a marker | Not present | Do not leave a scheme no operation uses; the authn profile adds it with its first protected operation |
| `openapi-validate` with kin-openapi in addition to Redocly | Redocly only | Redocly's `struct` rule validates structure; the document is also type-constructed by utoipa |
| `openapi-runtime-contract-check` as a separate `go test` selection | Part of `make test` | Cargo selects tests by package; the contract tests are ordinary tests of `service` and `infra-http` |
| `openapi-drift-check` by hashing generated Go files before and after `go generate` | A test embedding the committed file | The idiomatic snapshot check; it runs everywhere `make test` runs and needs no shell arithmetic |

## Deferred, with the change that reopens each

- Extractor rejection to `Problem`: axum's default `Json`/`Query`/`Path`
  rejections answer `text/plain`. The first operation with parameters or a
  body adds the mapping to `400`/`415`/`422` problems with `invalid_params`
  (RFC 6901 pointer for body members, `location.name` for parameters) and
  the boundary tests the Go README demands (happy path plus invalid path,
  query, body, and unknown field).
- Constraint enforcement (`pattern`, `minLength`, `maximum`): decide between
  `garde` or `validator` through `axum-valid` (schema attribute and
  validation attribute duplicated on the field) and newtypes with `TryFrom`
  (one source, hand-written `ToSchema`); decide with the first constrained
  parameter.
- The bearer security scheme, the global requirement, and the `401`/`403`
  problem components: the authentication profile.
- `x-extensible-enum` on `Problem.code` if a client needs the catalog
  machine-readably; today the catalog is documented by `Code::ALL` and its
  test.
- Publishing the document or serving Swagger UI: no consumer.
- `typify` for consuming an external contract: the bounded outbound HTTP
  profile.
- `vacuum` instead of Redocly: only if Node stops being available on every
  runner and workstation.

## Gotchas carried into implementation

1. `routes!(a, b)` groups methods of *one* path into one `MethodRouter`;
   two `GET` handlers on different paths in one `routes!` panic at router
   construction with "Overlapping method route" (*verified*). One
   `.routes(routes!(handler))` call per path.
2. `extensions(("x-security-decision" = json!({...})))` and `example =
   json!("ok")` in `IntoResponses` require the literal `json!` token; a path
   such as `serde_json::json!` is rejected by the macro (*verified*).
3. A schema referenced only through a `ToResponse` component is not
   collected automatically: register it in `components(schemas(Problem,
   InvalidParam))` or the `$ref` dangles (*verified*).
4. `routes!(module::handler)` resolves the generated `module::__path_handler`
   item; `pub(crate)` handlers in another module work (*verified*).
5. utoipa overwrites a schema silently when two types share a name (juhaku/utoipa#1154);
   use `#[schema(as = ...)]` for a colliding name. The drift test shows the
   result but does not name the cause.
6. `OpenApiRouter::new()` carries default `info`; always start from
   `OpenApiRouter::with_openapi(ApiDoc::openapi())` (juhaku/utoipa#1339).
7. utoipa `IntoResponses` and `ToResponse` derive documentation only; the
   runtime `IntoResponse` is hand-written and the contract tests assert the
   two agree for every declared status.
8. oasdiff treats a removed non-success status as non-breaking under
   `--fail-on ERR` (*verified* with `503` removed); a removed success status
   and a required property that became optional are errors (*verified*).
9. The first pull request adds `api/openapi/service.yaml`; the CI breaking
   step must skip when `git show BASE:api/openapi/service.yaml` fails.
10. `npx --yes @redocly/cli@<version>` downloads on first use; CI runners
    and workstations need Node, which `ubuntu-latest` provides.
11. `security()` (empty) renders `security: []`, the explicit public
    override; `security(())` renders `security: [{}]`, an anonymous
    alternative that the contract test classifies as neither public nor
    protected (*verified*). Redocly's `security-defined` rejects an
    operation with no `security` at all when the document has none either.
12. Inside `infra-http`, the local `health` module (handlers) shadows the
    `health` crate; the crate is written `::health::ReadinessReader`.
13. clippy's `doc_markdown` asks to backtick `OpenAPI` in doc comments;
    `clippy.toml` lists it under `doc-valid-idents` beside the defaults
    (`OpenTelemetry` is already in the default list).
14. utoipa takes a handler's doc comment as the operation description and a
    type's doc comment as the schema description: write those comments as
    contract text and put implementation notes in `//` comments.
