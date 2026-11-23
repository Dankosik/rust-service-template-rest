# ClickHouse decisions

<!-- template:begin clickhouse:docs-clickhouse-decisions -->
`crates/infra-clickhouse`, `crates/config`, `crates/service`, and `Cargo.toml`
(with [the clickhouse patches](../../vendor/clickhouse/PATCHES.md)) own the
supported API, timeouts, metric labels, and dependency pins. This record keeps
only rejected alternatives, external protocol and driver constraints,
empirical falsifications, rollout rules, and reopen conditions.

## Library and architecture choices

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| **D1:** `clickhouse` (`lz4`, `time`, `uuid`) fed custom `hyper-util` + `hyper-rustls` + `aws-lc-rs` transport over `klickhouse`, `clickhouse-arrow`, `clickhouse/rustls-tls`, `clickhouse/opentelemetry`, `clickhouse/inserter`, `clickhouse/zstd`, or runtime tuning toggles | `klickhouse` and `clickhouse-arrow` unconditionally require `chrono` (banned in `deny.toml` under `RUSTSEC-2026-0543`), and `clickhouse-arrow` pulls conflicting `opentelemetry-semantic-conventions`. `clickhouse/rustls-tls` hardcodes `.https_or_http()`, panics via `.unwrap()`, lacks custom root CA, mTLS, `rustls-platform-verifier`, `connect_timeout`, `TCP_USER_TIMEOUT`, and `.pool_timer(TokioTimer::new())` (disabling idle eviction in `vendor/hyper-util`). `clickhouse/opentelemetry`, `clickhouse/inserter`, and `clickhouse/zstd` pull duplicate `opentelemetry`, `quanta` (ignores `tokio::time::pause`), and C `zstd-sys`. Exposing `pool_idle_timeout`, compression, or `RowBinaryWithNamesAndTypes` validation as config keys adds dead configuration surface with no requirement to tune or disable them (`serde_derive_internals@0.29` via `clickhouse-macros` is gated in `deny.toml`, and `clickhouse-macros`/`clickhouse-types` clarify entries live in `build/docker/rust-notices.toml`). | Upstream `clickhouse` adopts the runtime and schema-validation bounds in `vendor/clickhouse/PATCHES.md` (retiring `vendor/clickhouse`), exposes HTTP `StatusCode` on `Error::BadResponse`, loses maintenance, or an accepted feature needs Native TCP/Arrow without `chrono` or runtime compression/validation toggles. |
| **A7 (reopened & retired via `vendor/clickhouse`):** Minimal audited `[patch.crates-io]` `vendor/clickhouse` over unpatched `clickhouse 0.15.3` | Initial non-vendoring assumption (`A7`) was empirically falsified by `crates/infra-clickhouse/src/tests/a7_stress.rs` and the ClickHouse reference audit: unpatched `InsertFormatted::abort`/`Drop` leaked detached `response.finish()` tasks and sockets; `collect_bad_response` buffered non-200 bodies without bound; `BufInsertFormatted::poll_flush_inner` masked early server rejections (`Code: 252`, `516`, `53`, `16`, `413`) and `ECONNREFUSED` as `Error::Network("channel closed")` on >254 KiB batches and fast warm-slot failures; `InsertMetadata::to_row` rejected `Nullable(T)` columns without explicit `DEFAULT`; `DataTypeValidator` ignored `DateTime64` precision and `Decimal` scale; `RowCursor` buffered headers/single rows before client byte checks; and internal `DESCRIBE TABLE` omitted `wait_end_of_query=1` and read-only cancellation. `vendor/clickhouse` applies strictly those seven surgical patches (see [PATCHES.md](../../vendor/clickhouse/PATCHES.md)). | Upstream `clickhouse` provides equivalent task abort, bounded error bodies, flush error propagation, `Nullable` default handling, `DateTime64`/`Decimal` precision/scale validation, streaming cursor byte caps, and `DESCRIBE` bounds, verified by `infra-clickhouse` regressions passing against the unpatched crate (delete `vendor/clickhouse`, root Cargo `exclude`/`[patch.crates-io]`, profile, `.dockerignore`, and Dockerfile carriers together per [PATCHES.md](../../vendor/clickhouse/PATCHES.md)). |
| **D2:** Concrete `feature-provider` facade without `service-failure`, `jobs-worker`, `migrate-clickhouse`, `fetch_stream`/`fetch_bytes`, `ManagedInserter`, or `AnalyticsRepository` trait | Coupling a `feature-provider` to `service-failure` breaks `InsertError::OutcomeUnknown` visibility if callers collapse errors via `?`; HTTP `Problem` projection and request-budget unconfirmed-insert tracking live at the `infra-http::clickhouse` boundary instead. No ClickHouse worker pipeline exists in the template; ClickHouse DDL is non-transactional and topology-specific (`MergeTree`, `ReplicatedMergeTree ON CLUSTER`, ClickHouse Cloud `SharedMergeTree`); bounded reads detect mid-stream failures before returning `Ok`; server-side `async_insert = 1` with `wait_for_async_insert = 1` avoids `TOO_MANY_PARTS` without risking silent loss of in-RAM buffered rows on `SIGKILL`; and `#[clickhouse(crate = "...")]` avoids a separate derive crate. | A feature requires streaming exports above `max_result_bytes`, iterator ingestion larger than in-memory slices, `jobs-worker` integration, or a second real ClickHouse wire adapter earns a port trait. |
| **D3:** Self-owned lifecycle (`start`/`close`), lock-free `ArcSwap` `password_file` refresh (5s polling + 1s-cooldown reactive reload on `Code: 516` / HTTP `401`), and non-gating readiness without startup ping, `trusted_network`, or JWT `access_token` | `crates/service` `bootstrap::Background` treats task error returns as fatal shutdown triggers, whereas unreadable or invalid `password_file` reloads must retain the previous valid credential and increment `clickhouse_password_file_refreshes_total{outcome="read_failed"}`. Reactive refresh shortens in-place rotation windows, while zero-downtime password rotation still requires ClickHouse dual-password overlap (`ALTER USER ... ADD IDENTIFIED ...`). Note that `clickhouse::Client` stores plain `String` credentials internally even though `ActiveCredentials` zeroizes its own copy. Gating `/health/ready` on an auxiliary OLAP store turns ClickHouse loss into a replica outage; production plaintext bypass and Cloud JWT auth are excluded in v1. | `crates/service` adopts a non-fatal background supervisor, an accepted operation cannot degrade when ClickHouse is unreachable, or a deployment requires `access_token` JWT auth or sub-restart client certificate reload. |
| **D4:** LRU-bounded `256` per-table `with_database` `Client` slots + `MetadataCoordinator` single-flight recovery with one pre-dispatch warm-slot re-prime over `clear_cached_metadata` + global mutex | Upstream `ClickHouse/clickhouse-rs#515` wipes metadata for all tables on `clear_cached_metadata()` and holds `RwLock::write().await` across network `DESCRIBE TABLE` without double-checking, causing cross-table write stalls and `N`-way `DESCRIBE` stampedes. Per-table isolated `InsertMetadataCache` slots share one Hyper pool while generation CAS, one automatic warm-slot re-prime on pre-dispatch `SchemaMismatch`, and synchronous `PrimeGuard` recover stale table schemas without stalling unrelated tables or surfacing a spurious HTTP 500 on the first evolved write. | Upstream `clickhouse` resolves `#515` with per-table double-checked single-flight metadata refresh and per-table invalidation. |
| **D5 & A4(7):** `wait_end_of_query = 1` + `buffer_size`, `readonly = 2` + `session_timezone = UTC` on client-managed reads, `QuerySettings::{Client, Server}`, separate read/write concurrency permits (`max_read_concurrency`, `max_write_concurrency`), aligned fractional timeouts & `max_memory_usage` on reads/writes/`DESCRIBE`, `fetch_one`/`fetch_optional` server `max_result_rows = 2`, two-phase write dispatch, canonical `collect_bad_response` fallback, and global `"clickhouse"` target denial | Without `wait_end_of_query = 1` and non-zero `buffer_size`, mid-query exceptions can arrive after HTTP `200 OK` headers commit (`clickhouse-rs#498`/`#538`). ClickHouse HTTP `POST` queries ignore `cancel_http_readonly_queries_on_client_close = 1` unless `readonly = 2` (or `readonly = 1`) is active; setting `readonly = 2` on `QuerySettings::Client` reads activates real server-side cancellation and blocks accidental mutations. Read-only users should prefer a ClickHouse profile with `readonly = 2` and `<constraints>` under `QuerySettings::Client`; rigid `readonly = 1` profiles reject per-query settings (`Code: 164`) and require `query_settings = "server"` with server-side cancellation and limits. Separate read and write semaphores prevent slow `SELECT`s from shedding `INSERT`s (and vice versa) while matching immediate `try_acquire_owned()` shedding. Fractional `max_execution_time`, `max_memory_usage`, and `wait_for_async_insert_timeout` align server budgets with remaining `OperationContext` deadlines. `QueryError::ResourceLimit` (`Code: 159`, `241`) and deterministic SQL errors (`Code: 16, 36, 43, 46, 70, 117, 153, 215, 386`) return `is_retryable() == false`. In `A4(7)`, `infra-clickhouse` checks numeric `Code: <u32>` (first 256 bytes) first, applies the four canonical empty/non-UTF-8 HTTP status strings only when `Code:` is absent, and fails closed all non-empty UTF-8 non-ClickHouse proxy prose to `Unavailable`/`OutcomeUnknown`. Denying target `"clickhouse"` in `infra-telemetry` blocks per-row `INFO` spans (`#533`) and `debug`-level `DB::Exception` dumps globally. | Upstream fixes `#498`/`#538`/`#533`, exposes `StatusCode` on `Error::BadResponse`, or exposes `Insert::sent_bytes()`, or a real L7 proxy deployment requires distinguishing non-empty UTF-8 proxy `401`/`403`/`413` prose from `Unavailable`/`OutcomeUnknown`. |
| **D6:** Composite `hyper-rustls` and `config-unknown-env-test` template profiles | Extracting `hyper-rustls` (`outbound_http=bounded \|\| clickhouse=http`) and `config-unknown-env-test` (`cache=redis \|\| clickhouse=http`) allows `--clickhouse http` without forcing `--outbound-http bounded` or `--cache redis`. | `outbound-http` and `clickhouse` no longer share `hyper-rustls`. |

## Protocol and type constraints (`Decimal`, `DateTime64`, `#325`)

`clickhouse 0.15.3` provides no `rust_decimal` feature (`ClickHouse/clickhouse-rs#325`),
and upstream `RowBinaryWithNamesAndTypes` validation accepts raw `i32`/`i64`/`i128`
without checking `Decimal` scale or `DateTime64` precision. `infra-clickhouse`
enforces exact scale and precision via `vendor/clickhouse` patch #5:

- **Decimals (`Decimal32`, `Decimal64`, `Decimal128`):** Features reading or
  writing ClickHouse `Decimal(P, S)` columns must use the scale-tagged types
  `infra_clickhouse::{Decimal32<SCALE>, Decimal64<SCALE>, Decimal128<SCALE>}`
  (and `Param::from(...)` / `Param::decimal(unscaled, scale)` for SQL
  parameters). Raw `i32`/`i64`/`i128` and scale mismatches fail validation with
  `SchemaMismatch` (`Encode` on write, `Decode` on read). `Decimal256` is
  unsupported.
- **Timestamps (`DateTime64`, `DateTime`):** Features reading or writing
  `DateTime64(P, ...)` columns must use the matching
  `infra_clickhouse::serde::time::datetime64::{secs, millis, micros, nanos}`
  (or `::option`) module; precision mismatches and raw `i64` fail validation.
  `Param::from(OffsetDateTime)` emits timezone-invariant Unix epoch text for
  timestamps `>= 1_000_000_000` (and UTC calendar text with
  `session_timezone = UTC` for earlier timestamps); use
  `Param::datetime_secs(OffsetDateTime)` when binding to 32-bit `{ts:DateTime}`
  to avoid `Code: 457` on subsecond timestamps.

Reopen when upstream `clickhouse` resolves `#325` with precision/scale-validated
`Decimal` and `DateTime64` Serde support that introduces no banned workspace
dependencies.

## Rollout and rolling schema evolution

1. **Retained-binary `Release Closure` order:** `service-config` rejects
   unknown top-level sections across `service`, `migrate`, and `jobs-worker`
   (`RETAINED_SECTIONS`), and `APP__CLICKHOUSE__*` environment variables or
   `--secrets-dir` files synthesize the `clickhouse` section during config
   loading. Deploy builds carrying `"clickhouse"` in `RETAINED_SECTIONS` across
   all retained binaries before populating `[clickhouse]` or
   `APP__CLICKHOUSE__*`, mount `APP__CLICKHOUSE__PASSWORD` / `password_file`
   exclusively into `service` (never into `migrate` or `jobs-worker`), and
   clear all `[clickhouse]` and `APP__CLICKHOUSE__*` inputs before rolling any
   binary back to a pre-ClickHouse release.
2. **Expand-contract rolling schema rule:** Because ClickHouse DDL is
   non-transactional and no ClickHouse migrator binary ships in v1, table
   changes across rolling deployments must use expand-contract: add new columns
   with `DEFAULT` or `Nullable` (`ALTER TABLE ... ADD COLUMN ... DEFAULT ...`
   or `Nullable(T)`, both admitted by `InsertMetadata::to_row`) before deploying
   new writers, let mixed-version writers recover stale metadata per table via
   `MetadataCoordinator` (automatic warm-slot re-prime on pre-dispatch
   `SchemaMismatch` and `Code: 53`/`16`/`117 (while reading header)`
   invalidation without blocking unrelated tables), and contract old columns only after all older writers drain. Never
   use in-place `MODIFY COLUMN` to change `Decimal` scale or `DateTime64`
   precision across live writers; add a new column and migrate via
   expand-contract instead. Reopen if a derived service must add non-defaulted,
   non-nullable columns across overlapping writer releases.
3. **Explicit deduplication and non-atomic batch failures:** `infra-clickhouse`
   never retries dispatched writes and targets ClickHouse 26.2+ unified
   `deduplicate_insert` (reopen if a deployment must target ClickHouse < 26.2).
   `AsyncInsertOptions` has no silent disabled default: callers must choose
   `with_token`, `with_payload_hash`, or explicit `without_deduplication`.
   Because ClickHouse only guarantees insert atomicity within a single block
   and single partition, `Code: 252` (`TOO_MANY_PARTS`) maps to
   `NotApplied(TooManyParts)` only for single-row inserts and to
   `OutcomeUnknown(Unavailable)` (with `clickhouse.error.reason = "too_many_parts"`
   and `db.response.status_code = "252"`) for multi-row inserts. Callers
   receiving `InsertError::OutcomeUnknown(_)` must retry only with an explicit
   `insert_deduplication_token` (`BatchInsertOptions::deduplication_token` or
   `AsyncInsertOptions::with_token` / `with_payload_hash`) or domain
   reconciliation.
<!-- template:end clickhouse:docs-clickhouse-decisions -->
