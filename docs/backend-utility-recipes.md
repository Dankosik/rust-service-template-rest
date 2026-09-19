# Backend utility toolkit and recipes

The toolkit has two adoption levels: production code uses a library where it
replaces an existing mechanism; executable recipes cover useful operations
that the health-only service does not perform yet. Both compile against the
same workspace versions. The recipes are ordinary tests in the existing
`integration-tests` package, not an exported `common` crate or new endpoints.

[Library selection](backend-library-selection.md) owns the decision rules and
[the research synthesis](../specs/production-utility-toolbox/research/synthesis.md)
records this change's baseline and API evidence. Do not confuse a tested
recipe with a configured production cache, provider, validator or import API.

## Run and adapt

```bash
make build
make test                                     # includes utility recipes; no database required
make test-package PKG=integration-tests        # utility suite in the existing test package
cargo test --locked -p integration-tests --test utility_async
```

The PostgreSQL tests in the same package still require the `integration`
feature and the existing database target; see
[PostgreSQL validation](validation/postgres.md). The recipe HTTP client talks
only to a loopback `wiremock` server. No recipe calls an external API or
requires credentials. Files are confined to `tempfile` directories.

Copy the relevant operation into its real owner and inherit the dependency
with `workspace = true`, enabling only the features it needs. Do not move all
the recipe dependencies into the service manifest. Normal dependencies are
selected per production crate; the rest are dev-dependencies. Default
workspace tests do compile the recipe toolkit: its compile-time cost is real,
even though it is not a kitchen-sink runtime dependency.

## Existing source migrations

| Owner | Before | Now | Contract retained |
| --- | --- | --- | --- |
| `health::Probe`, its test doubles and `PostgresProbe` | Handwritten `Pin<Box<dyn Future + Send>>` and `Box::pin` in each implementation | `async-trait` and ordinary `async fn` | Dynamic dispatch and boxed Send futures; no claim of removed allocations. Probe order and first-error short circuit remain sequential. |
| `Readiness::refresh_until` | Repeated nested cancellation `select!` blocks | One `CancellationToken::run_until_cancelled` around timer and probe work | Cancellation drops in-flight evaluation; no detached probe task. A pre-cancelled token starts no work. |
| Tokio metrics reporter | A cancellation-only select around the reporter | The same `tokio-util` cancellation adapter | Reporter lifetime remains owned by the tracked background task. |
| `Code` | Handwritten serde `Serialize` delegation | `derive_more::Display` delegates to `as_str`; `serde_with::SerializeDisplay` delegates to Display | The metadata match remains the only wire-name authority. `InternalServerError` still serializes to `internal_error`. |
| `Problem` | Three repeated optional-field omission attributes | `serde_with::skip_serializing_none` | Omitted fields never become null; retry metadata stays skipped, Utoipa schema and committed OpenAPI stay unchanged. |
| Admitted PostgreSQL TLS modes | Separate handwritten string-to-enum and enum-to-string matches | `strum::EnumString` and `IntoStaticStr` | Exact kebab-case whitelist only; no `allow`, `prefer`, case folding, repeated key or new connection source. |
| Catalog and router tests | Manual HashSet bookkeeping and mutable sorting vectors | `itertools::all_unique`, `sorted` and `collect_vec` | Independent expected values and catalog consistency checks remain. |
| OTLP one-poll test | Pin, noop waker, Context and Poll setup | `FutureExt::now_or_never` | Exactly one poll, not a blocking executor or a new runtime. |
| HTTP request-ID tests | UUID length only | `uuid::Uuid` parsing, version and variant checks | Valid incoming IDs still pass through; replacements must actually be RFC4122 UUIDv4 values. |

`run_until_cancelled` does not poll a future when already cancelled. When
work and cancellation become ready in the same later poll, Tokio-util prefers
work completion; this is the documented adapter behavior, not a strict
cancellation-wins guarantee. The existing refresh, staleness and shutdown
checks remain, with additional pre-cancellation and first-error-order tests.

## Collections and construction

[Executable cases](../test/tests/utility_collections.rs)

| Library | Ready-made operation | Use boundary |
| --- | --- | --- |
| `itertools` | Group pairs, count frequencies, preserve first occurrence while deduplicating, sort an iterator | Prefer a named library operation over a custom collection helper. Keep a simple business loop when it reads more clearly. |
| `indexmap` | `IndexMap`/`IndexSet` combine lookup and insertion order | Use `shift_remove` when relative order must survive deletion; `swap_remove` has different semantics. |
| `derive_more` | Display and borrowed access for a newtype | Checked construction remains `TryFrom`; do not derive an unchecked conversion or mutable access that bypasses invariants. |
| `bon` | Required inputs and optional/defaulted builder members | Selected builder for genuinely complex construction. Simple struct literals and `Problem::new(code)` remain appropriate. |

## Serialization, validation and document updates

[Executable cases](../test/tests/utility_data.rs)

| Library | Ready-made operation | Use boundary |
| --- | --- | --- |
| `serde_with` | Nested `DisplayFromStr`, millisecond durations, `double_option`, duplicate-key refusal | Preserve the external format and unknown-field policy. PATCH missing/null/value are three states. Do not replace trim-and-empty normalization with `NoneAsEmptyString` and lose trimming. |
| `validator` | Derive ordinary request-field constraints | Recipe uses a DTO-specific safe mapper to `Problem`, never the library error params containing submitted values. Production integration must add request ID and schema agreement. |
| `json-patch` | JSON Merge Patch and fallible JSON Patch | The operation authorizes fields. Apply fallible patches to a candidate and publish only on success; do not expose partial mutation. |
| `insta` | Review a stable JSON result as an inline snapshot | Keep semantic assertions. Do not add a second snapshot authority for committed OpenAPI or redact the relationship under test. |

`serde_with` also has `OneOrMany` and `StringWithSeparator` for external
representations that require them. Those are not reasons to make your own
public API accept ambiguous shapes. HTTP extraction and validation are
separate steps: the first production DTO still owns its rejection mapper,
RFC6901 pointer escaping, body limits, safe messages and OpenAPI constraints.

## Async work, buffers, retries and local caching

[Executable cases](../test/tests/utility_async.rs)

| Library | Ready-made operation | Use boundary |
| --- | --- | --- |
| `futures-util` | `buffered`/`buffer_unordered`, stream transformations and `try_collect` | Bound fan-out without a spawned task per item. Select input order vs completion order explicitly. Early failure drops local futures, not already completed external effects. |
| `tokio-util` | `ReaderStream`, `StreamReader`, cancellation and `TaskTracker` | No manual byte-pump or cancellation wrapper. Close the tracker and observe task completion. `codec` is the first choice for a real framed protocol. |
| `bytes` | Shared byte slices and explicit buffer encoding/decoding | Keep framing and bounds explicit; the example is not an unbounded body collector. |
| `backon` | Bounded retry with an eligibility predicate | Only the owning operation can decide idempotency. Bound total time, handle cancellation and never retry `CommitUnknown` blindly. |
| `moka` | Capacity, TTL, single-key initialization and invalidation | Process-local only. A configured TTL in the recipe is not a test of wall-clock expiration or distributed consistency. |

## HTTP and URLs

[Executable cases](../test/tests/utility_http.rs)

| Library | Ready-made operation | Use boundary |
| --- | --- | --- |
| `axum-extra` | Repeated query parameters and typed HTTP headers | `#[serde(default)] Vec<T>` handles zero/one/many occurrences. Parsing Authorization would not authenticate a token. Test-local routers do not publish a production error policy. |
| `url` | Encoded query pairs and URL joining | Joining an absolute URL can change the origin. URL parsing is not an allowlist or an SSRF defense. |
| `reqwest` | Reusable configured client and JSON decoding | Explicit connect/total budgets and redirects. Inspect native retry before adding another loop; the recipe disables retries and proxy inheritance. Production chooses TLS/provider roots. |
| `wiremock` | Loopback request/response contract and expected call count | Tests outbound clients, not replacement for inbound-router, socket or process-lifecycle tests. |
| `axum-test` | Test-local router requests and response assertions | Existing raw-body/concurrency tests remain where exact low-level behavior is under examination. |

`axum-extra::WithRejection` is available when a conversion alone is sufficient.
It does not supply request extensions to `From`; it cannot by itself populate
the service's request ID. Do not install another extractor framework just to
wrap it. The existing production HTTP architecture remains authoritative.

## Text, files and CSV

[Executable cases](../test/tests/utility_text_io.rs)

| Libraries | Ready-made operation | Use boundary |
| --- | --- | --- |
| `heck` | Acronym-aware snake/kebab case | Initializer or code-generation names. For serde names, use `rename_all` first. |
| `regex` | Compiled text patterns | Reuse a compiled pattern; do not replace a clear ASCII predicate or `split_once`. |
| `bstr` | Byte lines/search without lossy UTF-8 conversion | Binary or mixed-text input, not compulsory for normal JSON strings. |
| `unicode-normalization`, `unicode-segmentation` | NFC and grapheme boundaries | Product policy decides normalization and the meaning of length. Never silently change identifier equality. |
| `strsim` | Edit-distance suggestions | Suggestions, not fuzzy authorization or identifier equality. |
| `fs-err`, `walkdir` | Path-aware I/O errors and recursive traversal | Retain typed error identity where needed. Collect traversal errors instead of dropping them with `filter_map(Result::ok)`. |
| `tempfile` | Scoped files/directories and memory-to-disk spooling | Set resource limits and an appropriate directory; async services move blocking file work off worker threads. |
| `camino` | UTF-8 path contract | Reject non-UTF-8 paths explicitly, not through lossy conversion. Do not replace all OS paths. |
| `csv` | Serde import/export with delimiters, quotes and embedded newlines | Format parsing does not validate a business row or decide spreadsheet-formula handling. |

## Encodings and value types

[Executable cases](../test/tests/utility_values.rs)

| Libraries | Ready-made operation | Use boundary |
| --- | --- | --- |
| `base64` | URL-safe encoding/decoding | Encoding is neither encryption nor authentication. Choose the alphabet and padding contract explicitly. |
| `proptest` | Bounded encoding round-trip and alphabet properties | Independent properties supplement, not replace, explicit invalid-input cases. |
| `semver` | Version requirements including prerelease rules | No handwritten dot splitting or lexical comparison. |
| `ipnet` | CIDR parsing and address membership | Network membership is only one input to an authorization policy. |
| `uuid` | Parsed identity and UUIDv4 generation | UUID syntax does not establish authorization or request provenance. |
| `rust_decimal` | Checked decimal arithmetic and explicit string serialization | Finite precision, overflow and rounding remain explicit. Verify SQL and OpenAPI representations for a real field. |
| `time` | RFC3339 timestamp serialization and parsing | Selected default time model. Do not introduce additional time representations without a requirement. |

## Alternatives and capabilities intentionally not duplicated

| Earlier candidate or capability | Decision |
| --- | --- |
| `data-encoding` | `base64` is selected for the current encoding recipe. Add the alternative only for a real multi-alphabet requirement. |
| `chrono`, `jiff` | `time` is selected here; calendar/time-zone requirements may justify Jiff or an integration may require Chrono. Do not add three equivalent timestamp models. |
| `garde`, `axum-valid` | `validator` exercises the ordinary DTO case. Garde remains an alternative for complex models; verify exact trait-version compatibility and request context before adding a bridge. |
| `typed-builder`, `derive_builder` | `bon` is the selected constructor mechanism. Runtime-fallible completeness is a different API, not an additional mandatory builder. |
| SQLx `FromRow` and query macros | Keep SQLx and its first-repository/offline-metadata plan. There is no business query to fabricate merely to enable macros. Existing migration and commit-outcome policies stay intact. |
| SeaORM | Only reconsider for an actual CRUD-heavy derived service; no second schema authority or bypass of the transaction seam. |
| `stdext`, Git-sourced `stdx`, `tap` | No extra standard-library facade or style-wide syntax sugar. Prefer std and the specific crates above. |
| `anyhow`, `scopeguard` | No blanket replacement of typed errors or async lifecycle. A future CLI or synchronous cleanup may justify them independently. |
| `testcontainers` | Keep the established Compose plus SQLx test-database setup, not two environment managers. |
| `config`, `thiserror`, `tower-http`, Utoipa and telemetry adapters | Keep the already adopted implementations and their extension points. Custom secret-source, RFC9457 and lifecycle policy is not removed for a line-count target. |

The recipes deliberately show boundaries as well as happy paths. Their role
is to prevent new handwritten `StringUtils`, `CollectionUtils`, file walkers,
query parsers and async pumps, while keeping domain policy in the actual
service. A future feature inherits the narrow dependency, not this whole test
package.
