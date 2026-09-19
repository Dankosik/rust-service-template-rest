# Library-first cleanup: implementation decisions

The audit used merged PR #29 at
`9b10ac55be131a24f39302c4df15f1af8ed806c5`. Implementation starts from main
`ea6d805de5242914284919e5a71a099f0afb73a7`, after PR #30. Its renamed
`traces` module, typed sampler, readiness reader and shutdown ownership are
preserved. This is a behavior-preserving cleanup, not a new roadmap stage.

## Confirmed replacements

| Audit item | Owner and applied replacement | Contract retained |
| --- | --- | --- |
| 1. Default resource detectors | `crates/infra-telemetry/src/traces.rs::resource`: `Resource::builder()` replaces the boxed detector vector and `with_detectors`. | SDK detectors first, typed identity last; omit blank instance ID. |
| 2. Readiness ownership | `crates/health/src/lib.rs`: store `watch::Sender` directly, create it with `Sender::new`, borrow the previous evaluation while folding. | Clones share publication; writes work without readers; probe order, thresholds, staleness and cancellation stay unchanged. |
| 3. Migration history summary | `crates/migrate/src/lib.rs`: `applied_summary` uses `Iterator::max` and `len` instead of collecting and sorting version copies. Both history reads and `RunResult` use the summary. | Empty history gives `None`; count is not deduplicated; retain `saturating_sub`, lock/read/run/read/unlock order and error observations. |
| 4. HTTP response composition | `crates/infra-http/src/problem.rs`: axum `Json`, response parts and `Extension(code)`; numeric `HeaderValue::from`. `crates/infra-telemetry/src/metrics.rs`: response header tuple. | Exact Problem/Prometheus media types, serialized envelope, access-log extension and whole-second Retry-After with minimum one. |
| 5. Secret value traits | `crates/service/src/bootstrap/mod.rs`: clone the secret directly. Derive `OtelExporterConfig::default`; use `SecretString::default` for the PostgreSQL DSN and trace test options. | Secret bytes, redaction, serde defaults and application-specific pool defaults. |
| 6. Borrowed SQLSTATE | `crates/infra-postgres/src/transaction.rs::sqlstate`: SQLx `as_database_error` and the original `Cow<str>`. | Both callers keep the existing retryable/unknown-commit classification; no retries are introduced. |
| 7.1. Scalar query | `test/tests/postgres.rs::show`: `query_scalar`; remove unused `Row`. | Same SHOW whitelist, first column and decoded String. |
| 7.2. Test error conversions | `test/tests/postgres.rs::AppError`: `derive_more::From`; skip the Business variant. | Exactly the existing TxError and sqlx::Error conversions, without a new unit conversion. |
| 7.3. Fixture writes | `crates/config/src/load.rs::tests::write`: `std::fs::write`. | Same path, complete contents and create/truncate semantics. |
| 7.4. Process environment | `crates/service/tests/lifecycle.rs::Service::spawn`: `Command::envs`. | Supplied pairs still override base environment settings in order. |
| 7.5. Empty wrapper | Remove the migration test helper that only forwarded to `Migrator::with_migrations`; call the SQLx constructor at both sites. | Keep the separate migration fixture helper, which supplies meaningful fixture defaults. |

Problem currently serializes only strings, integers and their containers; the
response composition relies on that infallibility. Adding fallible custom
serialization requires an explicit safe error response decision: an outer
status tuple can override the status from `Json`'s serialization failure.

## Dependencies and primary API references

No package is added or upgraded. Cargo.lock and Rust 1.98.1 remain unchanged.
The sole manifest change enables `from` on the existing `derive_more` 2.1.1
dev-dependency in the integration-test package, not in production owners.
Its declared MSRV is 1.81 and license is MIT. The feature forwards to the
already selected derive implementation; it does not request a new library
family or enable all derives. This is less handwritten code, not a measured
build-time or runtime speedup. The full resolved feature graph was not run
in the authoring environment; locked CI remains the compilation check.

- [OpenTelemetry SDK 0.32.1 Resource builder](https://docs.rs/opentelemetry_sdk/0.32.1/opentelemetry_sdk/struct.Resource.html#method.builder)
- [Tokio 1.53.1 watch Sender](https://docs.rs/tokio/1.53.1/tokio/sync/watch/struct.Sender.html)
- [axum 0.8.9 response composition](https://docs.rs/axum/0.8.9/axum/response/index.html)
- [http 1.5.0 HeaderValue conversions](https://docs.rs/http/1.5.0/http/header/struct.HeaderValue.html)
- [secrecy 0.10.3 SecretString traits](https://docs.rs/secrecy/0.10.3/secrecy/type.SecretString.html)
- [SQLx 0.9.0 database-error accessor](https://docs.rs/sqlx/0.9.0/sqlx/enum.Error.html#method.as_database_error)
- [SQLx 0.9.0 query_scalar](https://docs.rs/sqlx/0.9.0/sqlx/fn.query_scalar.html)
- [derive_more 2.1.1 From and variant exclusion](https://docs.rs/derive_more/2.1.1/derive_more/derive.From.html)
- [derive_more 2.1.1 feature, license and MSRV declarations](https://docs.rs/crate/derive_more/2.1.1/source/Cargo.toml)
- [Iterator::max](https://doc.rust-lang.org/std/iter/trait.Iterator.html#method.max), [fs::write](https://doc.rust-lang.org/std/fs/fn.write.html), [Command::envs](https://doc.rust-lang.org/std/process/struct.Command.html#method.envs)

## Conditional candidates deliberately not applied

`RequestBodyLimitLayer` is not a drop-in equivalent of `enforce_body_limit`:
[tower-http 0.7.1](https://docs.rs/tower-http/0.7.1/src/tower_http/limit/service.rs.html)
uses the smaller of Content-Length and the configured limit while reading the
body. The current implementation uses the configured limit after early header
rejection. It also owns the Problem response integration. Adopting the layer
needs an accepted contract change or a demonstrated smaller equivalent adapter;
relocating the same mechanism to another wrapper is not sufficient.

Passing previously read TOML to `config::File::from_str` could remove a second
file read, but does not by itself preserve source-path diagnostics, error
classification and pre-merge secret rejection. No simpler equivalent full
replacement was established, so the loader mechanism is unchanged. The fixture
write cleanup above affects tests only.

DSN admission, secret-source policy, public error classification, readiness
policy, ordered shutdown and migration locking remain intentional application
code. They are not missed migrations. Existing library recipes do not justify
new runtime endpoints, retries, caches, clients or generic utility wrappers.

## Validation boundaries

Reuse the existing workspace build and test suite, HTTP contract tests,
configuration tests, and database CI selected by these paths. The readiness
regression covers publication across cloned owners with no readers and after
one owner drops. The Problem tests retain status/media-type/extension assertions,
compare serialized bytes, and cover absent, zero, fractional and maximum
Retry-After values. Existing migration tests cover empty history, applied
counts, repeated runs and failure observations.

No new runner, integration environment or workflow is introduced. Actual
commands, CI links, outcomes and unavailable checks belong in the pull request;
this decision record does not claim that authoring or static review compiled
the changes.
