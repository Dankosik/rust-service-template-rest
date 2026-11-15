# Cache decisions

<!-- template:begin cache:docs-cache-decisions -->
`crates/infra-cache`, `crates/config/src/cache.rs`, and `Cargo.toml` own the
supported API, timeouts, metric labels, and dependency pins. This record keeps
only rejected alternatives, external protocol/server constraints, and reopen
conditions.

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| `redis` multiplexed client over `fred`, `rustis`, `valkey-glide`, or connection pools (`deadpool-redis`, `bb8-redis`) | `fred` stalled upstream; `rustis` has a narrow user base; `valkey-glide` lacks a native crates.io API; pools add checkout queues unnecessary for pipelined bytes-only commands. | `redis` loses maintenance, or an accepted feature requires blocking commands or `WATCH`/`MULTI` session affinity. |
| Owned connection supervisor over `redis::aio::ConnectionManager` or streaming credential refresh | `ConnectionManager` retains detached reconnect tasks without caller cancellation and leaves timed-out slots on silent peers; driver streaming auth exits permanently on `AUTH` rejection and logs raw server text. | Upstream `redis` ships bounded cancellation, TLS provider injection, and sanitized retryable credential refresh. |
| Valkey as the tested server (RESP3 `HELLO 3`), standalone TCP | Valkey is BSD-3-Clause (Redis 8 moved to AGPLv3/RSALv2/SSPLv1); RESP3 push notifications isolate disconnect retirement per generation. | An accepted deployment requires Redis < 6.0 (no `HELLO 3`), Sentinel, or Cluster topology. |
| Distributed bytes-only cache beside `moka`, never a readiness gate or generic `Cache<K, V>` | Replica count alone does not require shared state when `moka` suffices; gating readiness on a cache turns cache loss into a full service outage. | An accepted operation cannot degrade when shared cache state is unreachable. |
| Startup-admitted client TLS material (`client_cert_path` / `client_key_path`) | Valkey/Redis TLS defaults to `tls-auth-clients yes`; validating the key pair at startup avoids lazy dial failures, while rolling restarts handle certificate rotation without background file watchers. | Client certificates rotate faster than rolling restarts can follow. |
<!-- template:end cache:docs-cache-decisions -->
