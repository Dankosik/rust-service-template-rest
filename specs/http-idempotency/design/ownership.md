# HTTP idempotency: Ownership Map V1

Status: ready. Behavior: [Specification](../spec.md). Mechanisms, public
signatures, SQL, formats, and proof architecture: [system design](system.md).
Baseline: `d24d1737d1647d9bfdf2b15ecb258630ec9cd326`; the current `main` at
`ac6a6ffe248cf6dac5232438876c564030316ed1` changes no path below. Existing
paths name current owners. New paths are proposed files, written as code
rather than links.

## Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner and exact action | Dependency, composition, or generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| Inbound idempotency module facade | `crates/infra-http/src/lib.rs` exposes `authn` as a marker-scoped public module; `authn.rs` shows composition living in `infra-http`. | New directory module `crates/infra-http/src/idempotency/`. `mod.rs` holds the module docs, private child modules, and re-exports of the section 4 seam surface, including the opaque `Tx` from the store but never the store's `connection`. Marked `lib.rs` lines add `pub mod idempotency;`. | `infra-http` gains marker-scoped dependencies on `infra-idempotency-store` and `sha2`. Features reach the seam only through `infra-http`, as with `VerifiedPrincipal`. | The directory is removed whole; the markers go. | Module tests; `cargo-shear`. | A consumer needs the seam without `infra-http`. |
| Declaration and agreement | `authn.rs` `is_protected_operation` walks utoipa metadata as JSON. | `declaration.rs`: rules 1, 2, 4, 5, and 6 per tuple and per document; the reverse key rule; non-`true` values; component presence; the declared-versus-composed comparison; `AgreementError` with a static rule enum. Pure. The expected key name, pattern, and component names come from `openapi.rs`. | Reads generated metadata only. Rule 3 is `protect`'s acceptance at `route`, and `protect`'s private rules are not copied. | None. | Inline table tests, one case per rule and direction. `crates/service/tests/openapi.rs` is the independent oracle. | A spec rule changes. |
| Composition and key handling | `protect` composes one tuple with `route_layer`. | `compose.rs`: `Composer` (`new`, `inert`, `route`, `components`, `agree`) and `Activation`. The key `route_layer` reads the deadline and principal, validates the key with the 400 Problem, counts `invalid_key`, inserts the attempt, and applies the 2xx wiring guard. `route` applies the key layer, then `protect`, keeping a clone of the tuple for the fail-closed form. | Calls `authn::protect` unchanged. The service composition root calls `route` and `components`. It is never a chain layer. | None. | Inline mounted tests with `Verifier::disabled()` and an inert store; factored tests of the key-rejection Problem and the wiring guard; P9. | Authentication composition changes shape. |
| Request identity | `request_id.rs` is the template-owned grammar precedent. | `identity.rs`: the `tchar` 1..=255 key grammar, and the scope digest over issuer, subject-or-client tag, `operationId`, and key, producing a `ScopeKey`. | Pure; `sha2` and the store's `ScopeKey` type. | None. | The pinned scope vectors (system design section 8); grammar tables covering quoted, comma, space, non-ASCII, empty, 256-byte, and repeated fields; a per-byte parity test against the character class of `openapi.rs`'s key pattern constant, which the test expands itself (no regex crate). | A grammar or scope change (Specification). |
| Semantic fingerprint | No encoder exists. | `fingerprint.rs`: `Fingerprint`, `FingerprintError` (static `Display` only; `execute` renders it), the canonical encoding 1 `Serializer`, and the digest. | `serde` and `sha2`. | None. | Literal encoding and digest vectors plus one case per data-model row. | Encoding change: Specification (stored format) and a compatibility plan. |
| Executor seam and outcomes | `RequestDeadline` (`harden.rs`); `SANITIZED_DETAIL` and `Problem` (`problem.rs`). | `execute.rs`: the `Idempotency` extractor, the `pub(super)` attempt type, `execute`, and the module-private (`pub(super)`) closed `Outcome` enum with its recorder (also used by `compose.rs`). It also holds the pure `Attempted`/`ReadBack` mapping, readback within the 100 ms reserve (its own constant), Problem construction (including the sanitized 500 with the request id for `Err(FingerprintError)`), 504 precedence, and the drop guard. The metric-name constant is declared here and re-exported. | Calls `Store::attempt` and `Store::read_back` only. The work receives only `Tx`. | None. | Inline tests for the pure mapping over fabricated store results; P9. | Store outcomes change. |
| Stored success format | None. | `stored.rs` captures a 2xx response. It buffers the body with `Limited`, filters the headers, applies both bounds, and encodes format 1 into the store's `Record` while keeping the captured form for the first response. It also decodes a record back into a replay response. The bound constants are declared here and re-exported. | Pure apart from buffering; uses `axum::http` types, `http-body-util`, and the store's `Record`. | None. | Round trip, exact-limit (including a body of exactly 1 MiB), over-limit, duplicate-value, order, and invalid-byte cases. | Stored-format change (Specification). |
| OpenAPI declaration helpers | `infra_http::problem::responses` holds the shared components. | `openapi.rs`: `IdempotencyKey` (`IntoParams`, header, exact schema); the four new response components (409 and 503 with an optional `Retry-After`); `IdempotentOperationProblemResponses` (the section 5 table); the private `OpenApi` derive that `Composer::components` seeds, the family's only registration path (system design section 2); and the `pub(super)` expected-value constants (key header name, pattern, length bounds, component names) with the inline test pinning the derive's literals to them. | Reuses `problem::responses` components by path. Doc comments are contract text. The `responses` module doc in `problem.rs` states the rule for a pack-owned family (unmarked). | None. | Pack assertions over the generated document; the openapi JSON walk. | A status or header rule changes. |
| Catalog codes | `problem.rs` holds the closed `Code` catalog, with authentication codes marker-scoped. | Add `IdempotencyRequestInProgress`, `IdempotencyKeyMismatch`, `IdempotencyUnavailable`, and `IdempotencyOutcomeUnknown`, with meta arms (wire, status, title, type URI per the spec table), inside `http-idempotency` markers. | Same crate as the seam. | Removed by markers. | The existing catalog uniqueness test plus seam metadata assertions. | A code change (Specification). |
| Record store | Nothing owns a profile table. `infra-postgres` owns no schema (Persistence). `PostgresProbe` shows the error-class style. | New crate `crates/infra-idempotency-store`. `lib.rs` is the facade and the `Store` handle (`new`, `inert`); `new` keeps whole microseconds of the retention (system design section 6.2). `attempt.rs` owns `attempt`, `read_back`, their four statements, the `ScopeKey`, `Digest`, `Record`, `WorkOutput`, `Attempted`, and `ReadBack` types, the opaque `Tx`, and `Tx`'s only accessor, the free function `connection`. `maintenance.rs` owns `check_startup`, `remove_expired` (the only drain), `run_cleanup` (the loop, beside the drain), their two statements, `StartupError`, and `CleanupError`. | Depends on `infra-postgres` (`in_tx_with`, `in_tx`, `retryable`, `TxOptions`), `sqlx`, `tokio`, `tokio-util`, `tracing`, `thiserror`. It knows no HTTP. `infra-http`, `service`, and `integration-tests` (dev) depend on it; no feature does. | Removed whole; nothing else names the table. | Inline tests (`lib.rs`: whole-microsecond retention; `attempt.rs`: lock-key literal, `TxError` classification); P1–P8, including a write under a retention with a sub-microsecond part. | Schema change, or a second consumer. |
| Retention configuration | `postgres.rs` is the section-file pattern; `app::occupied_string` is the non-secret vacancy helper. | New `crates/config/src/http_idempotency.rs`: `HttpIdempotencyConfig`, a vacant-when-empty duration built on `app::occupied_string`, 1 min..=30 days validation, and `required_retention(&PostgresConfig)`. Marked `lib.rs` regions add the module, export, field, and validate call. A marked loader test in `load.rs`. A commented example in `env/config/local.toml`. | No dependency on the pack; bootstrap passes values. | Path and markers. | Config crate tests, including a value with a sub-microsecond part. | A new knob (Specification). |
| Composition-root contract seam | `api.rs` `contract()` assembles one tree, and `document()` renders it. `api.rs:157` calls `contract()` inside `authn:service-api-protected-test-route`. | Marked additions: the `idempotency: &mut Composer` parameter on `contract`, a `.merge(idempotency.components())` chain line, a `&mut Composer::inert()` argument in `document()`, an import, and a separate `#[cfg(test)] mod idempotency_tests` with one test-only idempotent route. Unmarked: `api.rs:157` becomes `let document = document();`. No idempotency region sits inside the authentication region. Adopters add `.routes(idempotency.route(routes!(..)))`. | Contract assembly stays pure. The `openapi` binary is unchanged. | Markers leave today's `contract()`. | api.rs tests; openapi contract tests. | Adopters need a second composer. |
| Bootstrap activation and lifecycle | `mod.rs` owns the startup order. `prepare_auth`'s verifier is discarded today. `admit_and_serve` builds the contract after admission. | Marked additions: a `let verifier =` region immediately before the unchanged `authn:bootstrap-authn-prepare` region; `prepare_http_idempotency` after pools; a `Prepared` field, value, and destructure; the contract argument; `activate_http_idempotency` before `readiness.refresh`, with its private `start_http_idempotency` for the `Active` arm; error variants; and a test region inside the existing `mod tests` that calls `activate_http_idempotency` with `Inactive` and `start_http_idempotency` with `Store::inert()`. Unmarked: move `service::api::contract(..)` above admission and replace its comment with the pack-neutral text in system design section 10. | Bootstrap owns activation, spawning, and failure mapping. The store owns the checks and the loop. | The partial-startup path closes the pool; the tracker joins cleanup. | Bootstrap unit tests; existing lifecycle process tests (an inactive pack needs no value). | The startup order owner changes. |
| Contract proof | `crates/service/tests/openapi.rs` JSON-walks the protected rules. | A marked region: an independent walk of rules 1–6 and the reverse rule, fixture cases for its classifiers, and an agreement test (`Composer::inert()`, `contract(&mut composer)`, then `composer.agree(..)`). | Reads the generated document only. | Markers. | `make test`, `make openapi-check`. | — |
| Profile schema | `migrations/` is empty, and `migrations/README.md` says the first migration arrives with a feature. | New `migrations/20260923000001_create_http_idempotency_records.sql` (section 6.1). A marked README paragraph. The unmarked sentence at README lines 5-7 would be false with the pack, so it becomes "A service's own migrations arrive with its first durable feature; with an empty set, the runner proves the empty-history path." | Embedded by `migrate` (`build.rs` watches the directory). No history exemption. | Whole path. | `make migration-check`; P8 schema check; `the_embedded_set_runs_on_an_empty_database` still applies the set. | Schema change. |
| Real-PostgreSQL proof | `test/tests/postgres.rs` behind `integration`; `test/src/lib.rs` `dsn_for`. | New `test/tests/http_idempotency/main.rs` (P1–P8 at the store boundary and the helpers they use), `commit_proxy.rs` (the one-shot proxy used by both levels), and `mounted.rs` (P9 with every P9-only fixture). Marked dev-dependencies in `test/Cargo.toml`, split between the two profiles. A marked `test/README.md` line. | Uses the public store API, the public `infra_http::idempotency` seam, `infra_bearerauthn::test_support` (P9 only), and `sqlx`. | The directory is removed without the pack; `mounted.rs` and its dependencies are removed without `http-idempotency-mounted`. | `ALLOW_HEAVY=1 make test-integration-db`; the graph DB step. | A proof moves layer. |
| Profile selection and lock | `template_state.py`, `template_init.py`, `template_profiles.json`, `template_sync.py`, `make/template.mk`. | Add the `HTTP_IDEMPOTENCY` choice and refusals, the lock field and generations, marker selection, inventory sections, and sync validation (see "Initializer and validation owners"). One predicate in `template_state.py` owns the combination rule for both inputs and locks. | The public initializer keeps full metadata, formatting, and OpenAPI preflight. | Default removes the pack; no profile migration. | Init safety, sync canary, purity, projections. | A new value or shape. |
| Profile proof and delivery gates | `template-init-check.sh` (12 graphs), `template-profile-projections.py` (96), the CI initializer parts, `changed-surfaces.sh`, `verify.sh`, `make/source.mk`. | 16 graphs and 128 projections; the graph DB step; digests in receipts; a fourth CI part; classifier rows (below). | The existing runner, lock, and target sharing are retained. | — | Runner self-test; classifier and verify self-tests; the matrix itself. | A new dimension. |
| Adoption and architecture documentation | See "Documentation owners". | The new guide `docs/http-idempotency.md`, plus marked regions or shared edits in the listed owners. | No portable-manifest entry for pack documents; no portable instruction changes (system design section 3). | Guide and regions removed without the pack. | `make docs-check`; projections (links valid in every output). | — |

**Non-mechanical sources.**

- *Canonical encoder.* Template-owned code (reuse rung 4); the authority is
  system design section 8. The strongest rejected source is
  `serde_json_canonicalizer` 0.3.2, whose integers are lossy. Parity is proven
  by pinned vectors from an independent reference computation. Replace the
  encoder when a maintained crate meets section 2's criterion.
- *Composer and seam.* Repository reuse rung, with `authn.rs` as the precedent.
  They call `protect` unchanged and mirror its tuple walk. The strongest
  rejected alternative is a new chain layer, which would edit the hardened
  chain and see requests before authentication.
- *Store.* Built on the existing `sqlx` and `in_tx_with` extension points. The
  cleanup loop follows `record_metrics_periodically`. The strongest rejected
  sources are the crate survey's substitutes, which fail open or keep a
  separate store (see the synthesis).

## Files

| Path | Responsibilities | One present reason | Declarations and visibility | Call-path role | Lifecycle and error ownership | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `crates/infra-http/src/idempotency/mod.rs` | Inbound idempotency module facade | One public seam for services, features, and tests | `pub use` of the seam surface and the opaque `Tx`, never `connection`; private `mod` children | Entry for `service::api`, bootstrap, feature handlers | None | Its children; `infra_idempotency_store::Tx` | Logic, SQL, statics |
| `crates/infra-http/src/idempotency/declaration.rs` | Declaration and agreement | The spec's declaration rules are one authority: checked at composition and startup, mirrored by the contract test | `pub(super)` checks; `pub AgreementError` re-exported | `route` and `agree` | Returns `AgreementError`; never panics | utoipa, serde_json, openapi (expected-value constants) | Composition, runtime state, copies of `protect`'s rules or of the expected values |
| `crates/infra-http/src/idempotency/compose.rs` | Composition and key handling | The only way an operation becomes idempotent | `pub Composer`, `pub Activation`; private layer fn | `service::api` → `route` → `protect(key layer(handler))` | Records failures for `agree`; key and wiring Problems; `invalid_key` through the outcome enum | declaration, identity, openapi, execute (attempt type, outcome enum), `crate::authn`, `crate::problem`, `crate::request_id`, `crate::harden::RequestDeadline`, infra-bearerauthn (`Verifier`), infra-idempotency-store (`Store`), axum, utoipa, utoipa-axum, metrics, tracing | SQL, fingerprinting, chain edits |
| `crates/infra-http/src/idempotency/identity.rs` | Request identity | Pinned grammar and digest vectors | `pub(super)` validate and digest fns | Key layer | Pure | sha2, infra-idempotency-store (`ScopeKey`), std | I/O, logging values |
| `crates/infra-http/src/idempotency/fingerprint.rs` | Semantic fingerprint | Stable canonical bytes with pinned vectors | `pub Fingerprint`, `pub FingerprintError`; private serializer | Handler, then `execute` | Encoding errors become `FingerprintError`; no response rendering | serde, sha2 | Request parsing, I/O, Problems |
| `crates/infra-http/src/idempotency/execute.rs` | Executor seam and outcomes | The one-transaction flow as seen from HTTP, and its outcome table | `pub Idempotency`; `pub(super)` attempt type, `Outcome` enum, and mapping fns; the metric constant | Handler → `execute` → `Store::attempt` / `read_back` → response | Drop guard, readback bound, 504 precedence, Problem mapping (including `Err(FingerprintError)` with the request id); sets the "seam used" flag | stored, fingerprint, infra-idempotency-store, `crate::problem`, `crate::harden::RequestDeadline`, `crate::request_id`, axum, tokio, metrics, tracing | Composition, cleanup, SQL |
| `crates/infra-http/src/idempotency/stored.rs` | Stored success format | One versioned encoding, its capture, and its bounds | `pub(super)` capture, decode, filter; the bound constants | `execute` captures successes and decodes replays | Decode failure is integrity; capture failure is unstorable | axum::http, http-body-util, infra-idempotency-store (`Record`) | SQL, Problems |
| `crates/infra-http/src/idempotency/openapi.rs` | OpenAPI declaration helpers | Contract types adopters name in annotations | `pub IdempotencyKey`, `pub IdempotentOperationProblemResponses`, `pub` response components; `pub(super)` components doc and expected-value constants | Annotations; `Composer::components` | Documentation only | utoipa, `crate::problem` | Runtime behavior |
| `crates/infra-http/src/problem.rs` | Catalog codes; OpenAPI declaration helpers (the `responses` module-doc sentence only) | One closed catalog | Four marked `Code` variants and meta arms; an unmarked sentence in the `responses` module doc: a response family owned by one optional pack may live with that pack's seam and be registered by it | `Problem::new` in the seam | Unchanged | Unchanged | Idempotency logic |
| `crates/infra-http/src/lib.rs` | Inbound idempotency module facade | Registers the module | Marked `pub mod idempotency;` | — | — | — | — |
| `crates/infra-idempotency-store/src/lib.rs` | Record store | The provider crate's facade and handle | `pub Store` (`new`, `inert`); `pub use` of the module types and of `connection` | Bootstrap, the seam, store tests | Holds the pool and the retention in whole microseconds; no I/O on construction | its modules, sqlx (`PgPool`) | HTTP types, Problems |
| `crates/infra-idempotency-store/src/attempt.rs` | Record store | Arbitration, the execution transaction, and readback share the record statements and the `ScopeKey` and `Record` types | `impl Store { attempt, read_back }`; `pub` `ScopeKey`, `Digest`, `Record`, `WorkOutput`, `Attempted`, `ReadBack`; `pub Tx` (private field, derives only `Debug`) and `pub fn connection`; private statements | `execute` → here → `in_tx_with` | Maps `TxError` and SQLSTATE to `Attempted`; never retries; classes only in logs; inline tests pin the lock-key literal | infra-postgres, sqlx, tracing | HTTP, fingerprinting, Problems; any method, associated function, `Deref`, or conversion that reaches the connection through `Tx` |
| `crates/infra-idempotency-store/src/maintenance.rs` | Record store | The startup check and expired-record drain are maintenance of the same table | `impl Store { check_startup, remove_expired, run_cleanup }`; `pub StartupError`, `pub CleanupError`; private statements | Bootstrap activation; the spawned task | The loop exits on cancel; warns with a class | infra-postgres, sqlx, tokio, tokio-util, tracing, thiserror | Readiness, spawning, request paths |
| `crates/config/src/http_idempotency.rs` | Retention configuration | One section, one file | `pub HttpIdempotencyConfig`, `pub fn required_retention`; `pub(crate) fn validate`; private deserializer | Loader → `Config` → bootstrap | `ValidationError` naming the key | serde, humantime, humantime-serde, crate `app` (`occupied_string`), `validate`, `postgres` | Pack types, I/O |
| `crates/config/src/lib.rs` | Retention configuration | Registers the section | Marked `pub mod`, `pub use`, field, validate call | Loader | Unchanged | Unchanged | — |
| `crates/config/src/load.rs` | Retention configuration | Loader test for the environment key | Marked test | Test | — | — | — |
| `crates/service/src/api.rs` | Composition-root contract seam | The one route tree | Marked parameter, chain line, `document()` argument, import, test module; one unmarked test edit | Bootstrap, `openapi` binary, tests | Pure; no errors added | infra-http | Activation, I/O |
| `crates/service/src/bootstrap/mod.rs` | Bootstrap activation and lifecycle | The only startup-order owner | Marked private fns `prepare_http_idempotency`, `activate_http_idempotency`, and `start_http_idempotency`; `BootstrapError` variants; `Prepared` field; a test region inside `mod tests` | `serve` → `admit_and_serve` | Exit 1 before admission; spawn on the tracker | Existing plus infra-idempotency-store | Declaration rules, SQL |
| `crates/service/tests/openapi.rs` | Contract proof | Independent oracle over the committed contract | Marked tests and helpers | `make test` | Test-only | serde_json, rstest, service, infra-http | Production logic |
| `test/tests/http_idempotency/main.rs` | Real-PostgreSQL proof | P1–P8 and their helpers | Test crate root; `mod commit_proxy;`; marked `mod mounted;` | `cargo test --features integration --test http_idempotency` | Joins spawned tasks; bounded waits | integration-tests helpers, infra-idempotency-store, infra-postgres, sqlx, tokio | Production code; P9-only helpers |
| `test/tests/http_idempotency/commit_proxy.rs` | Real-PostgreSQL proof | Lost-acknowledgement injection around a real commit | `pub(crate)` proxy type with one-shot arm modes | P6 in both files | Stops its tasks on drop; each P6 test awaits its bounded join, like `finish_fixture` in `authn.rs` | tokio | Anything besides wire framing and the one fault |
| `test/tests/http_idempotency/mounted.rs` | Real-PostgreSQL proof | P9 with the real introspection verifier | Test module holding every P9-only fixture | Same target; introspection graphs only | The fixture server joins; the tracker closes | The above plus the unmarked infra-http, axum, axum-test (request driver), tokio-util (fixture tracker and token), serde, and serde_json, and the marked infra-bearerauthn (`test-support`), tokio-rustls (`aws_lc_rs`, `tls12`), secrecy, metrics, metrics-exporter-prometheus, utoipa (`macros`), utoipa-axum, and async-trait (the guide's port) | Verifier bypass, principal construction |

Do not split further for size. Inline tests stay beside each owner.

## Manifests and markers

`crates/infra-idempotency-store/Cargo.toml` inherits package and lints and has
no features. All dependencies use `workspace = true`:

- `infra-postgres`;
- `sqlx` with `postgres`, `runtime-tokio`, and `tls-rustls-aws-lc-rs`, the same
  set as `infra-postgres`;
- `thiserror`;
- `tokio` with `time` and `macros`;
- `tokio-util`;
- `tracing`.

Dev-dependencies are only what its unit tests use.

`crates/infra-http/Cargo.toml` gains, inside the `http-idempotency-dependencies`
marker, `infra-idempotency-store = { workspace = true }` and
`sha2 = { workspace = true }`. Its existing `axum`, `serde`, `serde_json`,
`http-body-util`, `metrics`, `tokio`, and `utoipa` features cover the seam;
no feature is added.

The root `Cargo.toml` gains, inside `http-idempotency` markers:

- `infra-idempotency-store = { path = "crates/infra-idempotency-store" }`
  (`workspace-http-idempotency-store`);
- `sha2 = { version = "0.11.0", default-features = false }`
  (`workspace-http-idempotency-digest`).

Cargo regenerates `Cargo.lock`: it gains the new local package and new
dependency edges of existing local packages, and no registry package. The
lock is never edited by hand.

| Marker profile | Selected when | Whole paths removed otherwise | Marked regions (file: ids) |
| --- | --- | --- | --- |
| `http-idempotency` | `HTTP_IDEMPOTENCY=postgres` | `crates/infra-idempotency-store/`, `crates/infra-http/src/idempotency/`, `crates/config/src/http_idempotency.rs`, `migrations/20260923000001_create_http_idempotency_records.sql`, `test/tests/http_idempotency/`, `docs/http-idempotency.md` | `Cargo.toml`: `workspace-http-idempotency-store`, `workspace-http-idempotency-digest`. `crates/infra-http/Cargo.toml`: `http-idempotency-dependencies`. `crates/infra-http/src/lib.rs`: `infra-http-idempotency-module`. `crates/infra-http/src/problem.rs`: `http-idempotency-codes`, `http-idempotency-code-meta`. `crates/config/src/lib.rs`: `config-http-idempotency-module`, `-export`, `-field`, `-validate`. `crates/config/src/load.rs`: `load-http-idempotency-environment`. `crates/service/Cargo.toml`: `service-http-idempotency-dependency`. `crates/service/src/api.rs`: `service-api-http-idempotency-import`, `service-api-contract-composer`, `service-api-idempotency-components`, `service-api-document-composer`, `service-api-idempotent-test-route`. `crates/service/src/bootstrap/mod.rs`: `bootstrap-http-idempotency-imports`, `-errors`, `-verifier`, `-composer`, `-prepared-field`, `-prepared-value`, `-destructure`, `-contract-composer`, `-activation`, `-functions`, `-tests`. `crates/service/tests/openapi.rs`: `service-openapi-http-idempotency-contract`. `test/Cargo.toml`: `test-http-idempotency-dependencies` (`infra-idempotency-store`). `test/README.md`: `test-readme-http-idempotency`. `env/config/local.toml`: `local-config-http-idempotency`. `migrations/README.md`: `migrations-readme-http-idempotency`. Documents: the ids under "Documentation owners". |
| `http-idempotency-mounted` | `HTTP_IDEMPOTENCY=postgres` and `AUTHN=oidc-introspection` | `test/tests/http_idempotency/mounted.rs` | `test/Cargo.toml`: `test-http-idempotency-mounted-dependencies`, holding every dev-dependency only `mounted.rs` uses: `infra-bearerauthn` with `test-support`, `tokio-rustls` with `aws_lc_rs` and `tls12` (as `infra-http` declares it), `secrecy`, `metrics`, `metrics-exporter-prometheus`, `utoipa` (`macros`), `utoipa-axum`, `async-trait`. Already-unmarked dev-dependencies (`axum`, `axum-test`, `tokio-util`, `infra-http`, `serde`, `serde_json`) stay unmarked. `test/tests/http_idempotency/main.rs`: `http-idempotency-mounted-module`. |

Region rules:

- Regions are non-nested and exactly match the inventory.
- A region sits beside an existing region, never inside it. Examples: a
  separate region after `docs-persistence-decisions`; the `let verifier =`
  region immediately before `authn:bootstrap-authn-prepare`; the
  `idempotency_tests` module beside the authentication test module. The
  existing call at `api.rs:157` gets the unmarked edit above rather than a
  nested region.
- Unmarked edits in service-owned files stay true in every output, with or
  without either profile, so they name no pack type, path, or profile. This
  covers the moved `contract(..)` call's comment (system design section 10),
  the `responses` module-doc sentence, and the unmarked documentation wording
  below. Template-owned files (`template-owned.paths`) carry no markers and
  are identical in every output. Like today's authentication and outbound
  rows, they may name profile values and pack paths, and they decide at run
  time from the lock or the tree.
- Renaming an id is a mechanical inventory edit, not a design change.

The source checkout without a lock still retains everything, as today.

## Initializer and validation owners

- `scripts/lib/template_state.py`:
  - Add `HTTP_IDEMPOTENCY_CHOICES = ("none", "postgres")`.
  - Add `http_idempotency_requirement(database, authn)`, the one owner of
    the combination rule: it names the unmet requirement (`database` or
    `authn`) or none. `validate_profiles` and `template_init.py`'s
    `parse_inputs` both call it and word their own refusals.
  - `validate_profiles` admits the four key sets of system design section 12
    and normalizes a missing field to `none`. It refuses
    `http_idempotency=postgres` when the predicate names a requirement.
  - Add `lock_has_explicit_http_idempotency` and `selected_http_idempotency`.
    In a lockless source, the latter reports `postgres` when
    `crates/infra-idempotency-store` exists.
  - `profile --field http_idempotency`.
- `scripts/lib/template_init.py`:
  - Inputs: `InitInputs.http_idempotency`, the `--http-idempotency`
    single-value flag, and the `HTTP_IDEMPOTENCY` environment variable,
    default `none`.
  - `parse_inputs` refuses unknown values, and both combinations through
    `http_idempotency_requirement`, with the section 12 messages.
  - Extend `profiles()` and `_selected_marker_profiles` (`http-idempotency`,
    plus the derived `http-idempotency-mounted`).
  - `_profile_data` admits a fourth inventory generation: the current keys
    plus the two new sections. A lock with `outbound_http` but without
    `http_idempotency` maps to the outbound generation, and `_replay` computes
    that flag.
  - Add a guarded feature-edge rule only if the equality oracle shows an edge
    that only the pack needs.
- `scripts/lib/template_profiles.json` gets the two sections from the marker
  table. `scripts/lib/template_sync.py` calls
  `selected_http_idempotency(target)` beside `selected_outbound_http`.
  `make/template.mk` sets `HTTP_IDEMPOTENCY ?= none` and exports it, like
  `OUTBOUND_HTTP`.
- `scripts/ci/template-init-check.sh`:
  - The graph loop and numbering from system design section 11.3.
  - Graph ids `1..16` in `validate_runtime_graphs` and the self-test.
  - Scrub `HTTP_IDEMPOTENCY`; pass `--http-idempotency`.
  - Service names `matrix-${database}-${authn}-${outbound_http}-${http_idempotency}-core`.
  - The Docker preflight.
  - The per-graph DB step recorded as `runtime-${graph}-idempotency-db`.
  - `openapi_sha256` and `cargo_lock_sha256` on each graph's receipt line.
  - `crates/infra-idempotency-store/`, `crates/infra-http/src/idempotency/`,
    and `test/tests/http_idempotency/` in the authorized candidate directories.
- `scripts/tests/template-candidate-paths.txt`: the three directories above,
  the config file, the migration, and the guide.
- `scripts/tests/template-profile-projections.py`:
  - The `HTTP_IDEMPOTENCY` dimension over the 16 admitted runtime selections,
    times 8 harnesses, gives 128 projections.
  - The 8 refused non-harness selections are asserted refused.
  - Runtime identity includes the new choice.
  - For every `none` selection: no registered path of either new profile, and
    no marker line or id of either profile.
  - Self-test faults for idempotency lock drift and tree drift.
- `scripts/tests/template-init-safety.py`: unknown-value, `DATABASE=none`, and
  `AUTHN=none` refusals leave the target byte-identical; default, selected,
  and historical-lock replay; five-field locks; a lock with
  `http_idempotency=postgres` and `database=none` or `authn=none` refused;
  profile migration refused.
- `scripts/tests/template-sync-canary.py`: no restoration of a pruned pack;
  historical lock shapes. `scripts/tests/template-owned-purity.py`: the two
  expected sections with `remove_when_unselected`.
- `make/source.mk`: help text "128 canonical projections … sixteen runtime
  representatives (13–16 also run their idempotency database suite; Docker)".
- `.github/workflows/ci.yml`: initializer part `http-idempotency` with
  `graphs: 13,14,15,16` and `REQUIRE_DOCKER: "1"` on the runtime step. The
  part restores the `database-postgres` cache key and skips the save step
  (system design section 11.3). Update the matrix comment. This change is
  routed through the CI/CD owner.
- `scripts/ci/changed-surfaces.sh`:
  - Add `crates/infra-idempotency-store/*` to the postgres `db_integration`
    case and to the source-only `initializer_runtime` list.
  - When `test/tests/http_idempotency/mounted.rs` exists, so P9 is retained,
    `crates/infra-http/*` and `crates/infra-bearerauthn/*` also select
    `db_integration` in the postgres case, because P9 mounts that code
    against a real database. The check follows the file-existence precedent
    of `make/source.mk`, and it covers the seam's own directory.
  - Add `crates/infra-http/src/idempotency/*` to the source-only
    `initializer_runtime` list, beside the existing
    `crates/infra-http/src/authn.rs` row.
  - Add `docs/http-idempotency.md` to the source-only projected-text list.
  - Pin these rows in the self-test.
- `scripts/ci/verify.sh`: reason text "canonical projections and sixteen
  runtime representatives"; `requires_docker=true` for
  `make template-init-check`; update its self-test expectations.
  `requires_heavy` stays `false` and `cost_class` stays `cpu`: `ALLOW_FULL`
  already gates the matrix and keeps it CI-owned by default (`ci_owned`),
  and a heavy flag would move that gate to `ALLOW_HEAVY`.

## Documentation owners

| Owner | Change | Marker |
| --- | --- | --- |
| `docs/http-idempotency.md` (new) | Adopter guide covering every topic in the spec's Guide list, plus: composition in `contract`; the port and adapter shape around `Tx` (section 3), where only the adapter calls `connection`, and it never issues transaction-control SQL or names the profile table; where the adapter is built (in `contract` when it holds no runtime resource, otherwise by bootstrap, as Integration Boundaries says); `READ COMMITTED`; the `HashSet` rule; the three-release `also_matching` rollout; the rollout order (migrate, set retention, deploy); that converting an existing operation protects retries only once every replica serves it through the boundary, and that a rollback or removal withdraws that protection at once, so the service promises clients the key only after the rollout; pool sizing | whole path |
| `docs/repo-architecture.md` | Invariant 1 states the reconciled rule from system design section 3: features' HTTP modules may use `infra-http`'s inbound contract surfaces, no feature depends on a provider crate, and no crate a feature depends on may depend on that feature | unmarked |
| `docs/architecture/boundaries.md` | Store owner row, edges, and composition note (`infra-http` composes idempotency as it composes authentication). The dependency-direction wording (the prose rule at line 79 and the diagram at line 53) is aligned with section 3. | `docs-boundaries-http-idempotency-owner`, `-edges`, `-composition`; general wording unmarked |
| `docs/architecture/http.md` | Idempotent composition (key handling inside authentication, `Composer::route`, agreement). The step-2 wording at lines 65-67 is aligned with section 3. Step 3 (lines 74-76) gains the general rule for a pack-owned response family. | `docs-http-idempotent-composition`; wording and rule unmarked |
| `docs/first-production-feature.md` | Several changes:<br>• a pointer to the guide, right after the outbound-profile pointer (line 29);<br>• the step-2 allowance names `infra-http`'s inbound contract surfaces;<br>• "nothing under `crates/infra-*` depends on it" (lines 34-35) becomes "no crate the feature depends on can depend on it";<br>• the response-component rule (lines 189-190) gains the pack-owned-family sentence;<br>• a note after the `contract()` snippet (line 235): with the pack retained, `contract` also takes the idempotency composer. | `docs-first-feature-http-idempotency` (the pointer, its own region after `docs-first-feature-outbound`) and `docs-first-feature-http-idempotency-contract` (the snippet note); wording and rule unmarked |
| `docs/project-structure-and-module-organization.md` | Placement rows for the seam, the store, and a feature's persistence adapter (an `infra-<provider>` crate); the `tests/<owner>/main.rs` form for a partially removable suite. In step 2, "(for the problem catalog and shared responses)" (line 77) becomes "(for its inbound contract surfaces)". | `docs-structure-http-idempotency-placement`; step-2 wording unmarked |
| `docs/architecture/integration.md` | None. Lines 18-22 already say a provider adapter maps into feature-owned types and bootstrap owns wiring. | — |
| `docs/architecture/persistence.md` | Profile table and statement ownership in the store crate; an idempotent operation's repository adapter joins the boundary's transaction through `Tx`, which the store opens with `in_tx_with`; the three deferral decisions retargeted. The unmarked "The migration set is empty until the first durable feature" (lines 199-201) becomes "The template ships no feature migration", which stays true with the pack. | `docs-persistence-http-idempotency`; the shared deferral bullet edited unmarked inside `docs-persistence-decisions` |
| `docs/backend-library-selection.md` | "First persistence repository" names the first feature-owned repository as the `query!` trigger | unmarked |
| `docs/architecture/runtime-lifecycle.md` | Activation before admission; the cleanup task and its join; the contract built before admission | `docs-lifecycle-http-idempotency`; one unmarked order sentence |
| `docs/configuration-source-policy.md` | `http_idempotency.retention` source and bounds | `docs-config-http-idempotency` |
| `docs/template-sync.md` | Selection and refusals; the five-field lock and historical shapes; sync; the 128/16 counts and the graph database step | `docs-template-init-http-idempotency`, `-sync`; lock and validation text unmarked |
| `docs/build-test-and-development-commands.md` | Counts; Docker for graphs 13–16 | unmarked counts; `docs-commands-http-idempotency` |
| `docs/ci-cd-production-ready.md` | Four initializer parts, including the Docker part and its restore-only cache, and the decision row | unmarked (source-template text) |
| `docs/validation/postgres.md` | The idempotency suite, the commit proxy, and the graph step | `docs-postgres-validation-http-idempotency`, after the postgres region |
| `migrations/README.md`, `test/README.md` | As in Responsibilities | ids in the marker table |
| `docs/roadmap.md` (source-only) | The stage-10 independence sentence (10.3 requires 10.1 and stage 8); stage status only at completion, by the delivery owner | unmarked |

No portable instruction changes: `.agents/skills/*` (including
`rust-coder` and `rust-structural-quality`) and `docs/spec-first-workflow/**`
keep their text (system design section 3). Documentation never links
`test/tests/http_idempotency/mounted.rs`, which is absent in JWT outputs, and
text about P9 in `test/README.md` and `docs/validation/postgres.md` says it
runs only where the introspection engine is retained (the source template and
`AUTHN=oidc-introspection` outputs), so it stays true in every output.

Implementation's final-validation owner runs:

- the matching build and tests;
- `make openapi-check`, `make migration-check`, and `make docs-check`;
- scoped shellcheck for changed shell;
- dependency and secret gates for the manifest and lock change;
- `ALLOW_HEAVY=1 make test-integration-db` and
  `ALLOW_FULL=1 make template-init-check`;
- the one-shot `none` equality comparison.

It adds no other aggregate, image, or deployment claim.
