# Backend boilerplate reduction: research synthesis

Reviewed on 2026-09-19 against main
`78a07898cdc26656d921220dd8bc5db19df7b44d`. The request is to implement the
applicable library recommendations and retain every conditional recommendation
without introducing unused features. The maintained decision guide is
[Backend library selection](../../../docs/backend-library-selection.md).

## Baseline and scope

The scaffold already uses Axum/Tower, Serde/config/Clap, thiserror, Utoipa,
SQLx, Tokio utilities and telemetry adapters. There is no business feature or
CRUD repository yet. The concrete remaining opportunities are repeated
ordinary HTTP-test plumbing, a manually maintained `Code::ALL` list, and
unnamed/table-driven validation and contract-test cases.

The HTTP failure envelope, configuration source/secret policy, staged process
shutdown and transaction commit-outcome classification are requirements, not
mechanical boilerplate to remove. This change preserves their owners and
behavior. It adds no HTTP operation, authentication placeholder, ORM, cache,
retry engine, configuration key or new validation gate.

## Immediate choices and alternatives

| Choice | Source and release inspected | Why selected over the existing mechanism | Cost and limit |
| --- | --- | --- | --- |
| `strum` 0.28.0, released 2026-02-22 | [Published crate](https://docs.rs/crate/strum/0.28.0), [VariantArray](https://docs.rs/strum/latest/strum/derive.VariantArray.html), [features](https://docs.rs/crate/strum/latest/features) | `VariantArray` derives a static slice from unit enum variants. `Code::ALL` remains an alias, but no longer requires a second manual variant list. A new local macro or a hand-maintained array preserves more machinery than the derive. | Only `derive` is enabled. The proc macro is a build-time dependency; no dynamic lookup is introduced. Preserve the exhaustive metadata match, wire format and const API. The derive is appropriate for this unit-variant enum, not arbitrary data-bearing enums. |
| `axum-test` 21.1.0, released 2026-08-20 | [Published crate](https://docs.rs/crate/axum-test/21.1.0), [TestServer](https://docs.rs/axum-test/latest/axum_test/struct.TestServer.html), [TestResponse](https://docs.rs/axum-test/latest/axum_test/struct.TestResponse.html) | Replaces repeated request building, response collection and JSON decoding in ordinary router tests. A Router uses in-process mock transport; `TestServer::new` returns the server directly, and automatic success assertions are off by default. Assertions still explicitly check status, headers, problem codes and sanitization. | Dev-dependency of `infra-http` only, default features disabled. Its mandatory cookies/multipart/JSON assertion machinery still adds dependencies. Raw bodies, framing, response extensions, connection behavior and concurrency tests retain their direct mechanism. It does not replace process tests or the bounded server. |
| `rstest` 0.27.0, released 2026-09-06 | [Published crate](https://docs.rs/crate/rstest/0.27.0), [feature list](https://docs.rs/crate/rstest/latest/features) | Named cases replace the OpenAPI security table loop without losing any of its eight cases. Configuration range/address cases become individually identifiable. Plain loops are still fine where an independently named case adds no value. | Dev-dependency of `service` and `service-config` only. Default features are disabled: synchronous parameterized cases need neither async timeout support nor crate-name discovery. No fixture framework is introduced. |

Release histories and upstream repositories provide maintenance signals, not a
guarantee of future support or a proof that most Rust backends use a crate.
`strum` and `rstest` also appear in the inspected Qdrant workspace manifest;
`axum-test` is an Axum-specific integration library rather than a claim of
whole-ecosystem dominance. Primary upstream repositories:
[strum](https://github.com/Peternator7/strum),
[rstest](https://github.com/la10736/rstest),
[axum-test](https://github.com/JosephLenton/axum-test).

## Observed Cargo resolution

A temporary, branch-only authoring workflow ran the repository's pinned
**Rust 1.98.1** toolchain. It intentionally resolved the added declarations
with Cargo, then inspected `cargo tree --locked --duplicates` and the feature
edges for all three libraries. The generated `Cargo.lock` was committed with
the manifests; it was not assembled or edited by hand.

[Dependency-resolution run](https://github.com/Dankosik/rust-service-template-rest/actions/runs/35406238288)
completed successfully. The metadata reported:

| Package | Selected version | Declared license | Declared Rust minimum |
| --- | --- | --- | --- |
| axum-test | 21.1.0 | MIT | 1.89 |
| rstest | 0.27.0 | MIT OR Apache-2.0 | 1.85.0 |
| strum | 0.28.0 | MIT | 1.71 |

The resolution added **30 package entries**; this is not a zero-cost change.
The test-only additions include `expect-json` and its numeric/date/typetag
support, cookies, multipart handling and rstest's macro support. The normal
production graph adds `strum` and its derive machinery. Axum remains 0.8.9,
Tower remains 0.5.3, and the OpenTelemetry family is not upgraded.

The duplicate-version view includes existing parallel major/minor versions
such as Syn 2/3 and tower-http 0.6/0.7. No blanket deduplication or unrelated
version upgrade is part of this change. Cargo also normalized three existing
Windows dependency references from the already present windows-sys 0.52.0 to
the already present 0.61.2; these are resolver-produced edges, not a manual
lockfile repair. Neither declared MSRV metadata nor dependency resolution
alone proves compilation; the PR's build/tests and dependency gates provide
the applicable evidence.

## Complete disposition of the remaining recommendations

The guide linked above records each trigger, the owning boundary and the
required safeguards. This is deliberate deferral until the first real use,
not missing implementation of an existing scaffold feature.

| Recommendation | Disposition |
| --- | --- |
| `validator` or `garde`, optionally `axum-valid` | Select one for the first constrained request. Preserve safe `Problem` mapping, request IDs, RFC 6901 paths and runtime/OpenAPI agreement. Check adapter versions; axum-valid 0.25's validator 0.20 trait is not validator 0.21. Do not serialize submitted values from validation errors. |
| `serde_with` | First nonstandard serialization or PATCH contract; retain all three missing/null/value states where required. Do not replace current duration/size adapters just to reduce a few attributes. |
| `derive_more` | First repeated standard-trait implementations on newtypes; never bypass checked construction or expose invariant-breaking mutation. |
| `bon`; alternatives `typed-builder`, `derive_builder` | First genuinely complex construction API. Compare compile-time and runtime completeness checks. Keep simple literals and the invariant-preserving `Problem::new(code)`. |
| `itertools` | First substantial collection operation not already clear with standard iterators or a loop. |
| SQLx `FromRow`, `query!` family and offline metadata | Use the existing persistence decision at the first real repository; `FromRow` is not compile-time SQL validation. No fictional query or new schema authority. |
| SeaORM | Reopen only for a measured CRUD-heavy derived service; preserve DSN admission, SQL migrations and commit-outcome policy. |
| `insta` | First meaningful snapshot result; no second OpenAPI snapshot or redaction of the relationship under test. |
| `reqwest` | First outbound HTTP adapter, with explicit budgets and TLS compatibility; inspect native retry policy first. |
| `backon` | First repeated eligible retries after the operation defines idempotency, error classes and total budget. Never blindly retry `CommitUnknown`. |
| `moka` | First bounded local cache with defined authority and invalidation; not a distributed cache or replacement for readiness's watch snapshot. |
| `wiremock` | First outbound HTTP contract tests; recheck current maintenance at adoption. |
| `proptest` | First useful parser/transformation property; keep explicit boundaries and bounded generation. |
| Existing config, errors, middleware, lifecycle and telemetry libraries | Retain and use their extension points. No generic repository, DI framework, custom derive suite or broad framework migration. |
| `testcontainers` | Do not add alongside the existing Compose plus SQLx test environment without a newly accepted requirement. |

## Validation and review boundary

The planned code change preserves production behavior except for deriving the
same enum variant list. Ordinary HTTP assertions remain visible; raw-body,
load-shedding, server and process-level tests remain at their existing level.
The OpenAPI classifier retains all eight security cases and the committed
OpenAPI file remains the sole expected generated artifact.

Use the existing repository commands and CI: formatting, workspace build and
tests for the manifest/lockfile change, the existing lint, license/advisory,
unused-dependency and documentation gates. Record observed results in the PR,
not as an unconditional success claim in this research note. The temporary
authoring workflow and script are removed from the proposed final tree; no CI
policy is relaxed. No percentage reduction or build/runtime speedup is claimed
without a measured comparison.
