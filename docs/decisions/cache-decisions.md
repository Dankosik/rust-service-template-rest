# Cache decisions

<!-- template:begin cache:docs-cache-decisions -->
`crates/infra-cache`, `crates/config`, and `Cargo.toml` own the supported API,
timeouts, metric labels, and dependency pins. This record keeps only rejected
alternatives, external protocol/server constraints, and reopen conditions.

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| `redis` multiplexed client over `fred`, `rustis`, `valkey-glide`, or connection pools (`deadpool-redis`, `bb8-redis`) | `fred` stalled upstream; `rustis` has a narrow user base; `valkey-glide` (0.10.0 on crates.io) is pre-1.0, little used, and brings its own reconnect supervisor; pools add checkout queues unnecessary for pipelined bytes-only commands. | `redis` loses maintenance, `valkey-glide` reaches 1.0, or an accepted feature requires blocking commands or `WATCH`/`MULTI` session affinity. |
| Owned connection supervisor over `redis::aio::ConnectionManager` or streaming credential refresh | `ConnectionManager` retains detached reconnect tasks without caller cancellation and leaves timed-out slots on silent peers; driver streaming auth exits permanently on `AUTH` rejection and logs raw server text. | Upstream `redis` ships bounded cancellation, TLS provider injection, and sanitized retryable credential refresh. |
| Own TCP/TLS dial handed to `MultiplexedConnection::new_with_config`, over the driver's `rediss://` dial | The driver rebuilds its rustls configuration on every TLS dial: it reads the OS trust store with blocking file reads on an async worker even when a private CA is configured, fails the dial on any unreadable OS certificate, and needs a process-wide default crypto provider. | The driver accepts a prebuilt rustls `ClientConfig` or connector for its own dial. |
| Every resolved address dialed at once, first connection wins (`select_ok`, as the driver's own dial does), over a staggered Happy Eyeballs dial | The cache holds one long-lived connection to one configured endpoint, so address-family preference and avoiding a few redundant connects buy nothing; a stalled first address is survived either way. | An accepted deployment resolves the cache name to enough addresses that the redundant connects matter to the server. |
| Valkey as the tested server (RESP3 `HELLO 3`), standalone TCP | Valkey is BSD-3-Clause (Redis 8 moved to AGPLv3/RSALv2/SSPLv1); RESP3 push notifications isolate disconnect retirement per generation. | An accepted deployment requires Redis < 6.0 (no `HELLO 3`), Sentinel, or Cluster topology. |
| Distributed bytes-only cache beside `moka`, never a readiness gate or generic `Cache<K, V>` | Replica count alone does not require shared state when `moka` suffices; gating readiness on a cache turns cache loss into a full service outage. | An accepted operation cannot degrade when shared cache state is unreachable. |
| Live `AUTH` on the open connection when `password_file` changes, over re-reading the file only for new connections (as `infra-postgres` does) | Valkey and Redis keep an authenticated session after its password changes ([ACL](https://valkey.io/commands/acl-setuser/)), so a re-read-only client would detect a bad rotated password only at the next reconnect, during an incident; token-file platforms such as [ElastiCache IAM](https://docs.aws.amazon.com/AmazonElastiCache/latest/dg/auth-iam.html) disconnect after 12 hours unless the session re-authenticates, and Lettuce re-authenticates on credential change for the same reasons. | No supported deployment rotates the password by rewriting a file, or the server drops sessions on every password change. |
| Startup-admitted client TLS material | Valkey/Redis TLS defaults to `tls-auth-clients yes`; validating the key pair at startup avoids lazy dial failures, while rolling restarts handle certificate rotation without background file watchers. | Client certificates rotate faster than rolling restarts can follow. |
| Atomic `EXISTS` and `GETRANGE` for bounded reads over plain `GET` or separate queries | Existing writers and earlier releases can leave large values. Checking after `GET` already allocated the reply. A range alone collapses empty and missing values. Both observations must describe the same value during replacement or expiry. | The server or driver offers one bounded read that distinguishes missing and empty while preserving the wire format. |

## Rollout

Grant the cache identity `MULTI`, `EXISTS`, `GETRANGE` and `EXEC` on its
existing key patterns before starting the new client. Retain the previous
read permission during mixed-version operation and rollback. Existing keys,
values and TTLs remain readable. A value above the new limit produces a size
error so the feature can use its source of truth.

Choose `cache.max_key_bytes` for the encoded namespace, separator and key.
Size `cache.max_concurrency` and `cache.max_value_bytes` together against
each replica's memory budget, allowing for protocol buffers, copies and
values still held by callers. Check peak RSS, lookup p99 and source load
under bursts and cache loss before promotion. Budget source capacity for
refused lookups and entries that exceed the value limit.

For a cache-aside read, reserve the time the source needs from the parent
operation's remaining budget. Skip the lookup when the reserve consumes that
budget. Otherwise pass a child context capped at the remainder to `get`.
After a miss, size refusal, or lookup timeout, check the original context
before calling the source. Propagate cancellation of the original operation.
<!-- template:end cache:docs-cache-decisions -->
