# sqlx-core 0.9.0: bounded native return

Upstream reviewed: 0.9.0

The published `sqlx-core-0.9.0.crate` archive has SHA-256
`05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb` and records
upstream revision `003b698e99e024f3621b8043a2426fde5b741171`.

`src/pool/connection.rs` bounds `floating.return_to_pool()` with a 5-second
timeout (matching unmerged upstream
[PR 4407 at `c3735ab`](https://github.com/transact-rs/sqlx/blob/c3735abf034689bb1fab9d7bf9f040e492de28cf/sqlx-core/src/pool/connection.rs))
so a wedged callback, ping, or graceful close drops the raw connection and
releases its pool slot. `src/net/socket/mod.rs` races resolved Tokio TCP
candidates concurrently, dropping losers once the first socket connects while
preserving `InvalidInput` on empty resolution and Tokio's resolver-order last
error when all candidates fail.

Retire this vendor directory, root Cargo patch/exclusion, and `postgres`
profile, Docker, and classifier carriers together when a published SQLx release
provides equivalent bounded pool return and TCP candidate fallback and passes
`make native-transport-regressions` and `ALLOW_HEAVY=1 make test-integration-db`.
