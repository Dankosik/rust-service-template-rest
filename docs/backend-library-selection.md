# Backend library selection

Use this guide when a feature needs a new dependency or starts repeating a
mechanical implementation. It records the boilerplate-reduction decisions,
not a bundle of dependencies every service must install. The
[research synthesis](../specs/boilerplate-reduction/research/synthesis.md)
records the inspected baseline and the evidence behind the immediate choices.

A dependency earns its place by deleting a current mechanism or a manually
maintained relationship. Prefer the standard library and existing dependencies
first. Declare a version once in the workspace, disable default features,
enable only the needed features in the consuming crate, and commit a
Cargo-generated lockfile. Recheck compatibility, maintenance, licensing and
advisories at adoption time; a recommendation is not a permanent approval of
all future versions. Do not hand-edit checksums or weaken dependency gates.

## Production utility toolkit

[Executable utility recipes](backend-utility-recipes.md) records the full
utility selection, runnable cases, actual source migrations and alternatives.
The recipes are normal tests in the existing test package; only production
consumers add normal dependencies. `Probe` now uses `async-trait`, cancellation
uses `tokio-util`, and `Code` serialization uses `derive_more` plus `serde_with`
without changing its wire identity.

## Adopted for existing code

| Library | Owner and use | Boundary |
| --- | --- | --- |
| `strum` with `derive` | `infra-http`: derive the complete `Code` variant array and keep `Code::ALL` as the public alias. | Keep the exhaustive `Code::meta()` match, wire names, `const` methods, statuses and URIs explicit. Do not derive serde `snake_case` Serialize: `SerializeDisplay` delegates through `Display` to `as_str` (`internal_error`, not `internal_server_error`). Do not derive unrestricted error policy. |
| `axum-test`, no optional features | `infra-http` dev-dependency: ordinary router requests, response decoding and assertions. | Use in-process mock transport. Keep raw `Request<Body>` and `oneshot` where byte framing, streaming body limits, concurrency or response extensions are the subject; keep real server/process tests for connection and lifecycle behavior. |
| `rstest`, no default features | `service-config` and `service` dev-dependency: named parameterized cases for range checks, addresses and the OpenAPI security classifier. | Keep semantic expectations visible in each case. Do not build a fixture framework for simple values or hide lifecycle setup. |

The examples live with their owners in `crates/infra-http/src/harden.rs`,
`crates/config/src/validate.rs` and `crates/service/tests/openapi.rs`. Test-only
libraries do not become normal dependencies of production crates.

## First request DTO or constrained parameter

Choose one validation library, not two. Start by evaluating
[`validator`](https://docs.rs/validator/latest/validator/) for ordinary REST
DTOs; prefer [`garde`](https://docs.rs/garde/latest/garde/) when enum and nested
container validation, explicit skipped fields or validation context make the
actual model simpler. Keep cross-field configuration budgets and domain
invariants with their existing owners rather than replacing them with opaque
custom attributes.

Before using a ready-made extractor such as
[`axum-valid`](https://docs.rs/axum-valid/latest/axum_valid/), inspect its exact
Axum and validator dependency versions. The researched `axum-valid` 0.25 line
uses `validator` 0.20, not 0.21, while its `garde` dependency is 0.23; do not
assume independently selecting the latest versions creates compatible traits.
A small transport-owned extractor is appropriate when the ready-made one
cannot preserve the service's error policy without a larger wrapper.

The implementation must preserve all of these contracts:

- Map extraction and validation failures to the existing `Problem` catalog,
  content type, status policy, request ID and `invalid_params` representation;
  JSON-body paths use RFC 6901 pointers, query/path parameters retain their
  documented locations. Do not turn body-limit failures into validation errors.
- Construct client-visible reasons from an allow-listed constraint description.
  Never serialize the validation library's error object or submitted values:
  `validator` can include the submitted value in its error parameters. Escape
  `~` and `/` when producing JSON-pointer segments.
- Keep `serde`, runtime constraints and `utoipa` schemas consistent. Deriving
  validation does not by itself prove OpenAPI constraints agree. Test the
  accepted boundary and rejected boundary, nested paths, unknown fields and
  secret-free error output for the new operation.

The first operation owns production integration. The executable HTTP recipe
exercises `validator` as a dev-dependency; it does not publish a generic
validator API, placeholder endpoint or authentication mechanism.
See [HTTP Architecture](architecture/http.md#adding-an-operation).

## Serialization, value types and construction

| Trigger | Preferred candidate | What must remain explicit |
| --- | --- | --- |
| Repeated custom serializers or a nonstandard external representation | [`serde_with`](https://docs.rs/serde_with/latest/serde_with/) adapters | The wire contract, unknown-field policy and OpenAPI shape. Do not replace working `humantime-serde` or `bytesize` merely for uniformity. It is now used for `Code` serialization and omission of optional `Problem` fields; the recipe suite also exercises nested adapters and PATCH states. |
| PATCH distinguishes missing, null and a value | `serde_with::rust::double_option` with the documented `default` and omission attributes | Missing means unchanged, null means clear, and a value means set. Test all three states and schema agreement; do not collapse them into `Option<T>`. |
| Repeated mechanical standard-trait implementations on newtypes | [`derive_more`](https://docs.rs/derive_more/latest/derive_more/), only the used derives | Preserve checked `TryFrom` construction. Do not generate `From<String>`, mutable dereferencing or setters that bypass a type's invariants. Keep `thiserror` for typed errors. |
| A genuinely complex constructor with required and optional inputs | Evaluate [`bon`](https://docs.rs/bon/latest/bon/) first | Preserve construction invariants and the actual API. Simple struct literals and `Problem::new(code)` remain appropriate. Never generate independent setters for status/title/URI derived from `code`. |
| A struct-only builder needs an alternative | [`typed-builder`](https://docs.rs/typed-builder/latest/typed_builder/) | Compare compile-time required-field checks and diagnostics against `bon` on the actual API; do not install both. |
| An existing API intentionally validates builder completeness at runtime | [`derive_builder`](https://docs.rs/derive_builder/latest/derive_builder/) | A fallible `build()` is a different contract, not an interchangeable type-state builder. Keep only one builder mechanism. |
| A compound collection operation replaces substantial manual plumbing | [`itertools`](https://docs.rs/itertools/latest/itertools/) | Prefer standard iterators and clear loops when equally direct. Do not replace a readable loop with a harder-to-follow combinator chain. |

## First persistence repository

Keep SQLx as the default. Use
[`FromRow`](https://docs.rs/sqlx/latest/sqlx/trait.FromRow.html) to remove
mechanical `Row::try_get` mapping when appropriate. `FromRow` alone does not
check SQL against the database at compile time. For suitable static queries,
use [`query!` and related macros](https://docs.rs/sqlx/latest/sqlx/macro.query.html)
with committed offline `.sqlx` metadata and the existing planned prepare check.
This is already a deferred decision in
[Persistence Architecture](architecture/persistence.md#decisions-recorded-here),
not a second migration framework or a query invented to justify tooling.

Consider [`SeaORM`](https://www.sea-ql.org/SeaORM/) only for an actual
CRUD-heavy derived service where entity/relation machinery removes enough
repetition. Compare concrete SQLx and ORM implementations first. Reconcile its
connection and transaction APIs with DSN admission, the existing migrations
and `CommitUnknown`; do not introduce a second schema authority or silently
bypass the transaction seam. Verify the selected release's SQLx compatibility
rather than assuming any SeaORM release can share the current pool.

## Outbound integrations and stateful capabilities

| First real need | Candidate | Required boundary |
| --- | --- | --- |
| Outbound HTTP adapter | [`reqwest`](https://docs.rs/reqwest/latest/reqwest/) | One reusable configured client, explicit request budgets, TLS/provider compatibility and safe errors. Inspect its native retry support before adding retry middleware. |
| Repeated eligible retry mechanics | [`backon`](https://docs.rs/backon/latest/backon/) | The operation owns idempotency, eligible errors, jitter/backoff, total deadline and cancellation. Never blindly retry `CommitUnknown`, non-idempotent effects, or stack independent retry loops. |
| Bounded process-local cache | [`moka`](https://docs.rs/moka/latest/moka/) | Specify capacity, expiration, invalidation and source of truth. It is not a distributed cache and does not make the existing readiness snapshot obsolete. |
| Tests of an outbound HTTP contract | [`wiremock`](https://docs.rs/wiremock/latest/wiremock/) | Local mock server, bounded waits and actual status/body/header behavior. Recheck maintenance at adoption; do not use it to replace inbound-router tests. |
| A complex stable output warrants a reviewed snapshot | [`insta`](https://docs.rs/insta/latest/insta/) | Assert important semantics separately. Redact only irrelevant nondeterminism; never hide the ID or timestamp relationship being tested. Do not create a second snapshot authority for the already committed OpenAPI document. |
| A parser/transformation has useful algebraic or grammar invariants | [`proptest`](https://docs.rs/proptest/latest/proptest/) | State the property independently of the implementation, retain useful explicit boundary cases, and bound generation. It is not mandatory for every helper. |

The recipe suite exercises the selected utility mechanisms without installing
a cache, HTTP provider, retry policy or new endpoint in the running service.
The owning feature/profile still supplies production policy and wiring.

## Retained mechanisms and rejected blanket changes

Keep `axum`, `tower`/`tower-http`, `serde`, `config`, `clap`, `thiserror`,
`utoipa`/`utoipa-axum`, SQLx, and the current telemetry and lifecycle stack.
Prefer their existing extension points before adding substitutes. No broad
framework replacement, generic repository, dependency-injection container,
custom derive collection, mandatory ORM, cache, or retry engine is justified
by the health-only scaffold.

The configuration loader retains source precedence and secret admission;
`Problem` retains the closed error policy and sanitized output; the HTTP
adapter retains framing, limits and fallback policy; transactions retain
commit-outcome classification. A shorter implementation that drops one of
these requirements is not a boilerplate reduction.

Keep Compose plus `#[sqlx::test]` for PostgreSQL tests. Do not add
`testcontainers` alongside the existing environment without a new requirement
that reopens the recorded persistence decision. Likewise, do not add a second
OpenAPI generator, snapshot, configuration loader or error library for a
mechanism the selected dependency already supplies.

## Acceptance for a library-driven refactor

Preserve existing assertions and runtime contracts; show which manual code or
synchronization point disappeared. Inspect the resolved feature graph and
duplicate versions, keep test support under dev-dependencies, and run the
build and workspace tests when manifests or the lockfile change. Keep existing
CI, advisory, license and unused-dependency gates intact. Report the actual
validation and any unavailable evidence; do not claim a percentage reduction
or performance gain without a measured comparison.
