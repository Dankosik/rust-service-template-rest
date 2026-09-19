# Production utility toolkit research and decisions

Baseline: `ac552f2a6e634187ba75a30e91bf551ecfa2e566` (main after PR #28).
Research/adoption date: 2026-09-19. This follows the broader utility research,
not a repetition of the earlier three-library test-helper change.

## Accepted outcome

Adopt the recommended utility toolkit, replace applicable existing manual
code and provide compiled, executable usage for common backend cases. Do not
invent business endpoints to claim every utility is a runtime dependency.

The [coverage matrix](../../../docs/backend-utility-recipes.md) accounts for
every recommendation and alternative. Production migrations are listed
separately from the six test-local recipe modules. The test package already
exists; no common facade, extra CI runner, framework or public utility API is
introduced. Recipes are normal workspace tests, so version incompatibilities
are visible to the existing build/test and unused-dependency gates.

## Exact mechanisms selected

### Dynamic async interfaces and cancellation

[async-trait](https://docs.rs/async-trait/latest/async_trait/) generates the
boxed Send futures already handwritten by `Probe` and each implementation.
The alternative `BoxFuture` alias only shortens the signature while leaving
manual boxing; native async methods without type erasure do not supply this
existing `dyn Probe` interface. Both trait and implementations carry the
attribute. No allocation or performance improvement is claimed.

[CancellationToken](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)
provides `run_until_cancelled`. It wraps the refresher's entire work loop,
including an in-flight evaluation, rather than cancelling only the sleep.
It starts no work for a pre-cancelled token. Later simultaneous completion is
biased toward the wrapped future, unlike a strict cancellation-wins policy.
The readiness snapshot, initial admission, first-error order, failure
threshold and staleness rules remain owned by `health`.

The same adapter replaces the reporter's cancellation-only select. The
[Futures extensions](https://docs.rs/futures-util/latest/futures_util/future/trait.FutureExt.html)
replace manual single-poll test machinery with `now_or_never`, not a blocking
executor. The batch recipe uses bounded streams but does not change readiness
into parallel probing.

### Serialization and mechanical mappings

[derive_more Display](https://docs.rs/derive_more/latest/derive_more/derive.Display.html)
and [SerializeDisplay](https://docs.rs/serde_with/latest/serde_with/derive.SerializeDisplay.html)
delegate to the existing `Code::as_str`. This removes the handwritten
Serializer implementation while retaining one wire-name authority.
`InternalServerError` remains `internal_error`; deriving serde snake_case
would be a regression. Metadata status/title/URI remains an exhaustive match.

[skip_serializing_none](https://docs.rs/serde_with/latest/serde_with/attr.skip_serializing_none.html)
is placed before the derives so Serde and Utoipa see the generated omission
attributes. Existing explicit `serde(skip)` retry metadata remains skipped.
The committed OpenAPI drift test must pass without regenerating a changed
contract just to accommodate a utility macro.

[Strum](https://docs.rs/strum/latest/strum/) generates both directions of the
private admitted TLS-mode mapping. Parsing is exact kebab-case, not case
insensitive; unsupported modes and duplicate DSN parameters remain failures.
The existing DSN pre-admission and error-redaction behavior stays explicit.

### Collections, typed data and utility examples

[Itertools](https://docs.rs/itertools/latest/itertools/trait.Itertools.html)
replaces auxiliary mutable sorting collections and hand uniqueness bookkeeping
in existing tests; the recipes exercise grouping/counting/order separately.
[IndexMap](https://docs.rs/indexmap/latest/indexmap/) is selected for combined
lookup and insertion order; its removal methods do not all preserve order.

[Axum-extra Query](https://docs.rs/axum-extra/latest/axum_extra/extract/struct.Query.html)
handles repeated query fields. The recipe uses defaulted `Vec`, not
`Option<Vec>`, and tests zero/one/many values. Typed headers replace textual
parsing in the recipe. WithRejection alone cannot supply request extensions
to a conversion, so it is not presented as the service's full Problem mapper.

[Serde-with](https://docs.rs/serde_with/latest/serde_with/) covers nested
adapters, duplicate-map refusal and three-state PATCH. The existing occupied
string helper also trims input: a generic empty-string adapter is not a
semantically equivalent replacement. [Validator](https://docs.rs/validator/latest/validator/)
is selected for the ordinary DTO recipe; its internal parameters may contain
submitted data. Client reasons are constructed from safe constraint text.

[Reqwest retry](https://docs.rs/reqwest/latest/reqwest/retry/index.html)
is inspected before introducing middleware. The local HTTP example disables
retry, proxies and redirects and sets time budgets. [Backon](https://docs.rs/backon/latest/backon/)
exercises a separately bounded eligibility policy; neither library decides
whether a committed or partially performed operation can be repeated.

## Coverage and alternatives

The executable recipes cover collections, newtypes/builders, serialization,
validation, JSON updates, snapshots, bounded async work, stream adapters,
cancellation, retries, local caches, query/header parsing, URL encoding, HTTP
mocks, text/Unicode, file traversal/spooling, CSV, encodings, semantic versions,
CIDR, UUID, decimal arithmetic and RFC3339 time values.

The matrix explicitly retains the alternatives: base64 over data-encoding,
time over multiple time models, bon over multiple builders, validator over
simultaneous validator/garde stacks, SQLx over a forced ORM. No real CRUD query,
calendar feature or new configuration source is fabricated to justify a crate.
Existing libraries and mechanisms that already solve their case are retained.

## Dependency resolution and proof

Versions are declared once with defaults off and features chosen by each
consumer. Cargo, not a handwritten lockfile, resolves the graph on the pinned
toolchain. The authoring run emits the exact selected versions, licenses,
minimum Rust versions and lockfile delta in `resolution.md`; those are
observations, not claims of a security audit. Inspect duplicate and feature
edges, especially TLS/OpenTelemetry families and production vs dev-only use.

Normal PR CI is the authority for `make fmt-check`, Clippy, workspace build,
workspace tests, unused dependencies, dependency/advisory/license checks and
document links. Because a PostgreSQL adapter and the test package change, the
existing selected database job must be observed separately. Compilation of a
database test is not a database-backed pass. New recipes require no production
credentials or external API. No benchmark, code-size percentage, cold-build
speedup or full supply-chain audit is claimed.

Temporary branch-only authoring files are removed before review; the final
change must not alter existing CI policy. The PR description records exact
observed results and remaining checks for its final head.
