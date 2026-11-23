# clickhouse 0.15.3: InsertFormatted task abort and bounded error responses

Upstream reviewed: 0.15.3

The published `clickhouse-0.15.3.crate` archive has SHA-256
`d6ad2112cf4ad543ceed12e48b779e82e8f3a8172374cac98d59e59feda35c69` and records
upstream revision `dad4c6dd8a40aaeaa7534abf38155219f6fd73b5`.

Two surgical runtime bounds are applied without adding dependencies or `unsafe`:

1. `src/insert_formatted.rs`: `InsertFormatted::abort` calls `handle.abort()` on
   both `InsertState::Active` (alongside `sender.abort()`) and
   `InsertState::Terminated` before transitioning state, so dropping or
   cancelling an in-flight `Insert` / `InsertFormatted` immediately terminates the
   spawned `response.finish()` task and drops its Hyper response future and TCP
   socket instead of leaking detached tasks and open connections. `NotStarted`
   and `Completed` paths remain unchanged.
2. `src/response.rs`: `collect_bad_response` caps both raw non-200 HTTP error
   body frame reads and decompressed error body collection at
   `MAX_BAD_RESPONSE_BYTES = 64 * 1024` (64 KiB), stopping early once the bound
   is reached while preserving leading bytes (`Code: <u32>`) and existing
   canonical `Error::BadResponse` status-reason fallbacks.

`.gitignore` adds `!/Cargo.lock` so the published archive `Cargo.lock` remains
tracked.

Retire this vendor directory, root Cargo `exclude` and `[patch.crates-io]`
entries, and profile, `.dockerignore`, and Dockerfile carriers together when an
upstream `clickhouse` release aborts `InsertFormatted`'s spawned response task on
`abort`/`Drop` and bounds non-200 error response body collection, verified by
`crates/infra-clickhouse`'s `a7_stress` and `infra-clickhouse` regressions
passing against the unpatched release.
