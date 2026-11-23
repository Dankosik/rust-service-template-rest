# ClickHouse decisions

<!-- template:begin clickhouse:docs-clickhouse-decisions -->
`crates/infra-clickhouse`, `crates/config`, `crates/service`, and `Cargo.toml`
(with [the clickhouse patch](../../vendor/clickhouse/PATCHES.md)) own the
supported API, timeouts, metric labels, and dependency pins. This record keeps
only rejected alternatives, external protocol and driver constraints,
empirical falsifications, rollout rules, and reopen conditions.

## Library and architecture choices

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| **D1:** `clickhouse` (`lz4`, `time`, `uuid`) fed custom `hyper-util` + `hyper-rustls` + `aws-lc-rs` transport over `klickhouse`, `clickhouse-arrow`, `clickhouse/rustls-tls`, `clickhouse/opentelemetry`, `clickhouse/inserter`, `clickhouse/zstd`, or runtime tuning toggles | `klickhouse` and `clickhouse-arrow` unconditionally require `chrono` (banned in `deny.toml` under `RUSTSEC-2026-0543`), and `clickhouse-arrow` pulls conflicting `opentelemetry-semantic-conventions`. `clickhouse/rustls-tls` hardcodes `.https_or_http()`, panics via `.unwrap()`, lacks custom root CA, mTLS, `rustls-platform-verifier`, `connect_timeout`, `TCP_USER_TIMEOUT`, and `.pool_timer(TokioTimer::new())` (disabling idle eviction in `vendor/hyper-util`). `clickhouse/opentelemetry`, `clickhouse/inserter`, and `clickhouse/zstd` pull duplicate `opentelemetry`, `quanta` (ignores `tokio::time::pause`), and C `zstd-sys`. Exposing `pool_idle_timeout`, compression, or `RowBinaryWithNamesAndTypes` validation as config keys adds dead configuration surface with no requirement to tune or disable them (`serde_derive_internals@0.29` via `clickhouse-macros` is gated in `deny.toml`, and `clickhouse-macros`/`clickhouse-types` clarify entries live in `build/docker/rust-notices.toml`). | Upstream `clickhouse` aborts `InsertFormatted`'s response task on `abort`/`Drop` and bounds `collect_bad_response` (retiring `vendor/clickhouse`), exposes HTTP `StatusCode` on `Error::BadResponse`, loses maintenance, or an accepted feature needs Native TCP/Arrow without `chrono` or runtime compression/validation toggles. |
| **A7 (reopened & retired via `vendor/clickhouse`):** Minimal audited `[patch.crates-io]` `vendor/clickhouse` over unpatched `clickhouse 0.15.3` | Initial non-vendoring assumption (`A7`) was empirically falsified by `crates/infra-clickhouse/src/tests/a7_stress.rs`: unpatched `InsertFormatted::abort`/`Drop` dropped the spawned `response.finish()` `JoinHandle` without `handle.abort()` (`30` cancelled inserts leaked `30` open server connections and `91` detached tasks; `10` local timeouts leaked `10` connections and `31` tasks), and upstream `collect_bad_response` buffered non-200 error bodies with unbounded `body.collect()`. `vendor/clickhouse` applies strictly two surgical bounds: `handle.abort()` on `Active` and `Terminated` in `InsertFormatted::abort`, and `MAX_BAD_RESPONSE_BYTES = 64 * 1024` across raw frame reads and decompression in `collect_bad_response` (preserving leading `Code: <u32>`, valid UTF-8/LZ4 prefixes, and canonical status fallbacks). | Upstream `clickhouse` aborts `InsertFormatted`'s response task on `abort`/`Drop` and bounds non-200 error body collection, verified by `a7_stress` and `infra-clickhouse` regressions passing against the unpatched crate (delete `vendor/clickhouse`, root Cargo `exclude`/`[patch.crates-io]`, profile, `.dockerignore`, and Dockerfile carriers together per [PATCHES.md](../../vendor/clickhouse/PATCHES.md)). |
| **D2:** Concrete `feature-provider` facade without `service-failure`, `jobs-worker`, `migrate-clickhouse`, `fetch_stream`/`fetch_bytes`, `ManagedInserter`, or `AnalyticsRepository` trait | Coupling a `feature-provider` to `service-failure` breaks `InsertError::OutcomeUnknown` visibility if callers collapse errors via `?`; HTTP `Problem` projection and request-budget unconfirmed-insert tracking live at the `infra-http::clickhouse` boundary instead. No ClickHouse worker pipeline exists in the template; ClickHouse DDL is non-transactional and topology-specific (`MergeTree`, `ReplicatedMergeTree ON CLUSTER`, ClickHouse Cloud `SharedMergeTree`); bounded reads detect mid-stream failures before returning `Ok`; server-side `async_insert = 1` with `wait_for_async_insert = 1` avoids `TOO_MANY_PARTS` without risking silent loss of in-RAM buffered rows on `SIGKILL`; and `#[clickhouse(crate = "...")]` avoids a separate derive crate. | A feature requires streaming exports above `max_result_bytes`, iterator ingestion larger than in-memory slices, `jobs-worker` integration, or a second real ClickHouse wire adapter earns a port trait. |
| **D3:** Self-owned lifecycle (`start`/`close`), lock-free `ArcSwap` `password_file` refresh, and non-gating readiness without startup ping, `trusted_network`, or JWT `access_token` | `crates/service` `bootstrap::Background` treats task error returns as fatal shutdown triggers, whereas unreadable or invalid `password_file` reloads must retain the previous valid credential and increment `clickhouse_password_file_refreshes_total{outcome="read_failed"}`. Gating `/health/ready` on an auxiliary OLAP store turns ClickHouse loss into a replica outage; production plaintext bypass and Cloud JWT auth are excluded in v1. | `crates/service` adopts a non-fatal background supervisor, an accepted operation cannot degrade when ClickHouse is unreachable, or a deployment requires `access_token` JWT auth or sub-restart client certificate reload. |
| **D4:** LRU-bounded `256` per-table `with_database` `Client` slots + `MetadataCoordinator` single-flight recovery over `clear_cached_metadata` + global mutex | Upstream `ClickHouse/clickhouse-rs#515` wipes metadata for all tables on `clear_cached_metadata()` and holds `RwLock::write().await` across network `DESCRIBE TABLE` without double-checking, causing cross-table write stalls and `N`-way `DESCRIBE` stampedes. Per-table isolated `InsertMetadataCache` slots share one Hyper pool while generation CAS + synchronous `DropGuard` recover stale table schemas without stalling unrelated tables. | Upstream `clickhouse` resolves `#515` with per-table double-checked single-flight metadata refresh and per-table invalidation. |
| **D5 & A4(7):** `wait_end_of_query = 1` + `buffer_size`, `QuerySettings::{Client, Server}`, two-phase write dispatch, immediate permit shedding, canonical `collect_bad_response` fallback, and global `"clickhouse"` target denial | Without `wait_end_of_query = 1` and non-zero `buffer_size`, mid-query exceptions can arrive after HTTP `200 OK` headers commit (`clickhouse-rs#498`/`#538`). `readonly = 1` profiles reject per-query budget settings (`Code: 164`), requiring `query_settings = "server"`. Immediate `try_acquire_owned()` shedding matches `infra-cache`/`infra-object-storage` without queueing inside the `8s` HTTP budget. In `A4(7)`, `clickhouse 0.15.3`'s `collect_bad_response` replaces the HTTP status with body text whenever the body is non-empty valid UTF-8, emitting canonical `"400 Bad Request"`, `"401 Unauthorized"`, `"403 Forbidden"`, and `"413 Payload Too Large"` only on empty/non-UTF-8 replies without `X-ClickHouse-Exception-Code`; `infra-clickhouse` checks ClickHouse numeric `Code: <u32>` (first 256 bytes) first, applies those exact four canonical strings only when `Code:` is absent, and fails closed all non-empty UTF-8 non-ClickHouse proxy prose to `Unavailable`/`OutcomeUnknown`. Denying target `"clickhouse"` in `infra-telemetry` blocks per-row `INFO` spans (`#533`) and `debug`-level `DB::Exception` dumps globally. | Upstream fixes `#498`/`#538`/`#533`, exposes `StatusCode` on `Error::BadResponse`, or exposes `Insert::sent_bytes()`, or a real L7 proxy deployment requires distinguishing non-empty UTF-8 proxy `401`/`403`/`413` prose from `Unavailable`/`OutcomeUnknown`. |
| **D6:** Composite `hyper-rustls` and `config-unknown-env-test` template profiles | Extracting `hyper-rustls` (`outbound_http=bounded \|\| clickhouse=http`) and `config-unknown-env-test` (`cache=redis \|\| clickhouse=http`) allows `--clickhouse http` without forcing `--outbound-http bounded` or `--cache redis`. | `outbound-http` and `clickhouse` no longer share `hyper-rustls`. |

## Protocol and type constraints (`rust_decimal` `#325`)

`clickhouse 0.15.3` provides no `rust_decimal` feature or Serde module
(`ClickHouse/clickhouse-rs#325`), and `RowBinaryWithNamesAndTypes` schema
validation matches `Decimal32`, `Decimal64`, and `Decimal128` columns exclusively
against signed fixed-width integers (`i32`, `i64`, `i128`). Features reading
or writing ClickHouse `Decimal(P, S)` columns must serialize and deserialize
explicit scaled-integer newtypes (`i32`, `i64`, or `i128`) at the feature
boundary rather than `rust_decimal::Decimal`. Reopen when upstream
`clickhouse` resolves `#325` with RowBinary-validated `Decimal` Serde support
that introduces no banned workspace dependencies.

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
   with `DEFAULT` or `Nullable` (`ALTER TABLE ... ADD COLUMN ... DEFAULT ...`)
   before deploying new writers, let mixed-version writers recover stale
   metadata per table via `MetadataCoordinator` (`SchemaMismatch` / `Code: 53`
   invalidation without blocking unrelated tables), and contract old columns
   only after all older writers drain. Reopen if a derived service must add
   non-defaulted columns across overlapping writer releases.
3. **Explicit deduplication on ambiguous writes:** `infra-clickhouse` never
   retries dispatched writes and targets ClickHouse 26.2+ unified
   `deduplicate_insert` (`disable` for `AsyncDeduplication::Disabled`, `enable`
   for `ByPayloadHash`/`ByToken` and `BatchInsertOptions::deduplication_token`;
   reopen if a deployment must target ClickHouse < 26.2). Callers receiving
   `InsertError::OutcomeUnknown(_)` must retry only with an explicit
   `insert_deduplication_token` (`BatchInsertOptions::deduplication_token` or
   `AsyncDeduplication::ByToken` / `ByPayloadHash`) or domain reconciliation.
<!-- template:end clickhouse:docs-clickhouse-decisions -->
