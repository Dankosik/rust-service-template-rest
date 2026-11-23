# clickhouse 0.15.3: surgical transport, streaming-bound, and schema-validation patches

Upstream reviewed: 0.15.3

The published `clickhouse-0.15.3.crate` archive has SHA-256
`d6ad2112cf4ad543ceed12e48b779e82e8f3a8172374cac98d59e59feda35c69` and records
upstream revision `dad4c6dd8a40aaeaa7534abf38155219f6fd73b5`.

Seven surgical runtime and correctness bounds are applied without adding
dependencies or `unsafe`:

1. `src/insert_formatted.rs` (`InsertFormatted::abort`): calls `handle.abort()`
   on both `InsertState::Active` (alongside `sender.abort()`) and
   `InsertState::Terminated` before transitioning state, so dropping or
   cancelling an in-flight `Insert` / `InsertFormatted` immediately terminates
   the spawned `response.finish()` task and drops its Hyper response future and
   TCP socket instead of leaking detached tasks and open connections.
2. `src/response.rs` (`collect_bad_response`): caps both raw non-200 HTTP error
   body frame reads and decompressed error body collection at
   `MAX_BAD_RESPONSE_BYTES = 64 * 1024` (64 KiB), stopping early once the bound
   is reached while preserving leading bytes (`Code: <u32>`) and existing
   canonical `Error::BadResponse` status-reason fallbacks.
3. `src/insert_formatted.rs` (`InsertFormatted::poll_ready`,
   `BufInsertFormatted::poll_flush_inner`): when `ChunkSender` reports closed,
   transitions to `InsertState::Terminated` and awaits `handle` via
   `poll_wait_handle_or_closed` instead of returning a generic
   `Error::Network("channel closed")`, preserving real server `Error::BadResponse`
   (`Code: 252`, `516`, `53`, `16`, `413`) and `hyper` connect errors across
   multi-chunk (> 254 KiB) batches and fast warm-slot failures.
4. `src/row_metadata.rs` (`InsertMetadata::to_row`): permits omitting
   `Nullable(T)` and `LowCardinality(Nullable(T))` columns even when ClickHouse
   `DESCRIBE TABLE` reports `default_type = ""`, matching ClickHouse SQL's
   implicit `NULL` default for `ALTER TABLE ... ADD COLUMN ... Nullable(T)`.
5. `src/types/decimal.rs`, `src/serde.rs`, and `src/rowbinary/{validation,ser,de}.rs`:
   adds scale-validated `Decimal32<SCALE>`, `Decimal64<SCALE>`, and
   `Decimal128<SCALE>` types, tags `serde::time::datetime64::{secs,millis,micros,nanos}`
   with exact precision markers, and enforces exact `DateTime64` precision and
   `Decimal` scale in `DataTypeValidator` (rejecting mismatches and untagged
   raw `i32`/`i64`/`i128` against `DateTime64`/`Decimal` columns).
6. `src/cursors/{raw,row}.rs` and `src/error.rs`: adds
   `RowCursor::with_max_decoded_bytes` and `Error::ResultBytesLimitExceeded`,
   enforcing the decoded-byte ceiling in `RawCursor::poll_next` before
   buffering headers or oversized single rows in `RowCursor`.
7. `src/lib.rs` (`Client::insert_metadata_query`): sets `wait_end_of_query=1`
   on the internal `DESCRIBE TABLE` query and, when `max_execution_time` is set
   on the client, sets `readonly=2` and
   `cancel_http_readonly_queries_on_client_close=1` on `DESCRIBE TABLE` without
   applying `readonly=2` to the subsequent `INSERT`.

`.gitignore` adds `!/Cargo.lock` so the published archive `Cargo.lock` remains
tracked.

Retire this vendor directory, root Cargo `exclude` and `[patch.crates-io]`
entries, and profile, `.dockerignore`, and Dockerfile carriers together when an
upstream `clickhouse` release provides equivalent bounds and schema checks,
verified by `crates/infra-clickhouse` regressions passing against the unpatched
release.
