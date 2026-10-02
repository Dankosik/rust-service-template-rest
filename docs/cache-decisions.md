# Cache decisions

<!-- template:begin cache:docs-cache-decisions -->
Stage 10.11 library and lifecycle decisions, recorded 2026-09-27; the RESP3,
replacement, and TLS-provider rows were added or revised on 2026-10-01, and
the password-file, warm-up, and `error_type` rows on 2026-10-02.
[Guide](cache.md) owns adoption and observable behavior. This record retains
the accepted choices and their reopen conditions.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| `redis` 1.7.1, default features off, only `tokio-comp`, `connection-manager`, `tokio-rustls-comp`, and `token-based-authentication` | `fred` 10.1.0 was last released and last pushed on 2025-02-27 (19 months) and had 43 open issues. `rustis` 0.26 has a small user base (30k recent downloads). `valkey-glide` has no crates.io Rust API; its core is used through FFI. | Pinned client and four features. TLS uses the process rustls provider and native roots, or a PEM at `root_ca_path`. `#insecure` fails because `tls-rustls-insecure` is not enabled. Reopen if `redis` fails this maintenance evidence, or a required command is absent. |
| `ConnectionManager`: one multiplexed lazy connection | `deadpool-redis` 0.23.1 (2026-08-26, `redis` ^1.6) and `bb8-redis` 0.26.0 (2025-12-09, `redis` ^1) are not needed. The multiplexed connection already pipelines concurrent requests. Pools pay off for blocking commands, `WATCH`/`MULTI`, or pub/sub, which this cache does not use. | No pool crate. Reopen with `deadpool-redis`, which is more current than `bb8-redis`, if a feature needs `WATCH`, `MULTI`, or a blocking command. |
| Distributed bytes-only client, not moka alone | `moka` 0.12.16 is already the process-local cache. It does not share state across replicas and cannot back distributed rate limits or locks. | A second mechanism beside `moka`. `moka` stays the per-process choice. Reopen this profile only if no deployment needs shared bytes. |
| Valkey as the tested server; Redis OSS-compatible for the command subset | Valkey is BSD-3-Clause, a Linux Foundation project, and a fork of Redis OSS 7.2.4. Redis 8 is AGPLv3 / RSALv2 / SSPLv1. `GET`, `SET` with `PX`, `DEL`, and `PING`, plus `HELLO 3` (with `AUTH` inside it), `CLIENT SETINFO`, and `SELECT` by the client, are identical in Redis OSS >= 7.2 and Valkey. | Standalone TCP only. Tested image `valkey/valkey:9.1.2-alpine@sha256:48332870af354a799964c0012ae1194a0bf2bf894eb508f945810596dc2d8d11`. Reopen Sentinel or Cluster only for an accepted topology requirement. |
| Compose, not testcontainers, for container tests | The repository already keeps Compose as the container environment and rejects a second runner beside it. The proof is the `valkey` service in `env/docker-compose.yml`. | Local and CI proof share that file. Reopen testcontainers only if Compose cannot host a required case. |
| No readiness gate | A gate would turn a cache outage into total unavailability and contradict degradation. | Startup still runs one probe inside 1 s, logs `cache_unavailable_at_startup` on failure, and continues. A service that requires the cache pushes `cache.probe()` into readiness. Never liveness. Reopen only if an accepted operation cannot degrade. |
| `cache.command_timeout` as configuration, default 100 ms, range 1 ms to 1 s, with `2 * cache.command_timeout <= http.request_timeout` | A constant cannot track the operator's request timeout. The rule leaves at least half of the request budget after one degraded call. It does not count a handler's calls: each sequential call can spend another `command_timeout`, so the feature budgets calls × `command_timeout` plus its own work and a response reserve. During an outage each call costs at most `command_timeout`. | One operator key. Connect stays the 1 s constant. Reopen a second timeout key only if connect and command must be tuned apart. |
| Connect, backoff, and TCP as constants | `connection_timeout` is 1 s. Backoff uses `min_delay` 100 ms, `exponent_base` 2, `max_delay` 2 s, and `number_of_retries` 6. TCP nodelay is on. Keepalive is 30 s, interval 10 s, and 3 retries where supported. Linux `user_timeout` is 10 s so a half-open connection is detected and reconnected. | Not operator keys. Reopen if a deployment cannot use these bounds. |
| Namespace name is the metric label and the key prefix (`{name}:{key}`) | Leaving keys unprefixed would let two features that share a server read each other's entries, and the metric label would not match the keyspace it measures. | A key written by another client must carry the same prefix to be shared. Reopen if a feature must read keys it does not own. |
| RESP3 always, whatever `protocol=` the DSN carries | Leaving the DSN default (RESP2) was the alternative. redis 1.7.1 wires the manager's disconnect watcher only when the protocol is RESP3 (`aio/connection_manager.rs`, the `supports_resp3()` branch of `new_lazy_with_config`). On RESP2 a connection dropped while idle is noticed only by the next command, which fails. A unit test hangs up an idle connection and sees the client dial again with no call; it fails on RESP2. | A server or proxy without `HELLO` (Redis before 6.0) is unsupported, and a refused `HELLO` is a plain server error (see the next row). Reopen if an accepted deployment endpoint cannot answer `HELLO 3`. |
| Replace a `ConnectionManager` after any error that is not an I/O error, at most once per 2 s | redis 1.7.1 reconnects only on I/O errors and the few kinds it calls unrecoverable (`aio/connection_manager.rs`, `reconnect_if_io_error` and `reconnect_if_dropped`). On RESP3 a refused `HELLO` (`WRONGPASS`, `NOAUTH`, a full client table) is returned as the raw server error (`connection.rs`, `check_resp3_auth`), which is neither, so the manager returns it forever; a regression test reproduces it with a server that refuses authentication until the client's retry chain ends. `server_error.rs` maps `ReadOnly` to `RefreshSlotsAndRetry`, so standalone mode never reconnects after `READONLY`; go-redis closes such connections (go-redis #790). A stored setup failure and a reply to one command are the same error to the caller, so the rule cannot be narrower than "not I/O". No configuration or supported hook in 1.7.1 changes this; redis-rs `main` on 2026-10-01 has the same code. No upstream redis-rs issue exists yet. | A small wrapper that swaps the manager behind a lock. A per-command server error such as `OOM` also costs one new connection per 2 s. Remove the wrapper when redis-rs reconnects after non-I/O setup failures and `READONLY`; report the gap upstream. |
| `infra-cache` installs the aws-lc-rs rustls provider when the admitted address is TLS | redis 1.7.1 builds its TLS configuration with `rustls::ClientConfig::builder()` (`connection.rs`) and accepts no provider or ready configuration, so it needs the process default. The workspace compiles both `ring` and aws-lc-rs, so rustls cannot pick one and panics on the first dial. Installing it in the composition root instead was rejected: every binary that holds a `Cache` would have to repeat it, and forgetting it is a panic on a lazy connection, not a startup error. | A library sets a process default, idempotently, with the same provider every other crate in the workspace passes explicitly. Reopen when redis-rs accepts a provider or a `ClientConfig`. |
| A replaced manager is driven by one background `PING`, bounded at 20 s | A manager from `new_lazy_with_config` holds an unspawned shared future (`aio/connection_manager.rs`), so its connection advances only while a caller awaits it; with sparse traffic and a 100 ms `command_timeout`, recovery took several calls. redis-rs spawns its own reconnect the same way (`reconnect`). `new_with_config` would connect eagerly but must be awaited, which a replacement under a lock on the command path cannot do. A unit test sees the new connection with no further call; it fails without the task. | One detached task per replacement, at most one per 2 s, that ends with its `PING` or after 20 s (longer than one reconnect chain). Its failure replaces nothing, so a failing server is still dialed only when traffic asks. Remove it with the wrapper. |
| `cache.password_file` through redis-rs's `StreamingCredentialsProvider` (`token-based-authentication`) | A static DSN password cannot follow a platform that rotates it, and IAM tokens (ElastiCache, Memorystore) expire in minutes. Alternatives: rereading the file in the replacement wrapper (template-owned reconnect logic, and no re-authentication of an open connection, which an expiring token needs); redis-rs `entra-id` (adds the Azure SDK for one provider); provider-specific token code in the template. The driver's provider is its supported extension point: each connection attempt takes the first item of a subscription, and an open connection sends `AUTH` for each later item (`aio/multiplexed_connection.rs`). The file is provider-neutral and matches `postgres.password_file`. The feature adds one edge (`log`) and no package. | The stream reads the file every 5 s; redis-rs subscribes twice per connection, so each connection repeats `AUTH` once after it opens. The template does not mint tokens: a sidecar or agent writes the file. Reopen provider-specific code only for a platform that cannot write a file. |
| Histogram only, with `error_type` on failed series | Hit, miss, and error counts are the `_count` series of `cache_operation_duration_seconds`. Labels are the closed sets `cache`, `operation` (`get`, `set`, `delete`), and `outcome` (`hit`, `miss`, `ok`, `error`, `timeout`, `cancelled`); `error` and `timeout` series add `error_type` (`timeout`, `io`, `auth`, `response`, `parse`, `other`). Without it the cause existed only on the span and a debug event, so at the production log level a refused password and a network outage were the same `error`; OpenTelemetry's `db.client.operation.duration` carries `error.type` for the same reason. | At most six more series per namespace and operation, registered on first use. No parallel counters. Reopen a counter only if an operator question cannot be answered from the histogram. |

Registry and maintenance evidence, fetched 2026-09-27: `redis` (redis-rs) 1.7.1, released 2026-09-25, BSD-3-Clause, MSRV 1.88, repository pushed 2026-09-25, ~26.6M recent downloads. `fred` 10.1.0, last release and last push 2025-02-27, 43 open issues. `deadpool-redis` 0.23.1, 2026-08-26, depending on `redis` ^1.6. `bb8-redis` 0.26.0, 2025-12-09, depending on `redis` ^1. `rustis` 0.26, 30k recent downloads. `valkey-glide` has no crates.io Rust API. `moka` 0.12.16 is already in the workspace. Valkey is BSD-3-Clause. Redis 8 is AGPLv3 / RSALv2 / SSPLv1. redis-rs MSRV 1.88 fits workspace Rust 1.98. These figures do not describe the resolved lock.

## Resolved dependency graph

The profile adds five packages to `Cargo.lock`: `infra-cache`, `redis`
1.7.1, `arc-swap` 1.9.2 (the connection manager's swappable connection),
`arcstr` 1.2.0 and `xxhash-rust` 0.8.18. `token-based-authentication` adds
the edge `redis` -> `log` 0.4 and `infra-cache` names `futures-util` for the
credentials stream; both were already locked. `combine` 4.6.8 was already locked;
it gains only the async feature edges `redis` needs. `socket2` 0.6.5 was
already locked; `infra-cache` names it directly because `redis` does not
re-export `TcpKeepalive`, which sets keepalive retries. `xxhash-rust` is BSL-1.0 (Boost Software License, OSI
approved); `deny.toml` allows it inside the cache marker. No existing package
changed version. `CACHE=none` projects the lock without these packages and
edges.

## Supported extension points

A later rate-limit or lock feature adds its operation to `infra-cache` beside
`get`, `set`, and `delete`, so it shares the connection, `command_timeout`,
observation, and manager replacement instead of holding a raw
`ConnectionManager`. Those capabilities are not in this profile. Readiness is an opt-in: composition pushes `cache.probe()`
into the probes in `crates/service/src/bootstrap/mod.rs`. Sentinel, Cluster,
and a connection pool stay closed. Reopen a pool with `deadpool-redis` when a
feature needs `WATCH`, `MULTI`, or a blocking command. Reopen Sentinel or
Cluster only with an accepted topology requirement.

## Ownership and proving surfaces

`infra-cache` owns admission, the lazy client, the password file's
credentials stream, namespace operations, the probe, and sanitized
observation. `service-config` owns the `[cache]` section.
Bootstrap owns connect, the bounded startup check, optional readiness
registration, and the shutdown drop. The feature owns keys, serialization,
TTL, invalidation, and any map from `Unavailable` to HTTP 503.

Unit tests cover the admission matrix (plaintext refused or allowed, missing
password refused or allowed, `#insecure` refused, unix socket refused, CA with
plaintext refused, missing CA file, unparsable CA, a password file with and
without a DSN password, a missing or empty password file), `CacheError` Display and
Debug never containing the password, namespace name validation, `error.type`
mapping, observation labels, a refused `HELLO` reported as `auth` and
retried after the client gives up, `READONLY` reconnecting to the new
primary, an idle connection that was dropped reconnecting with no call, a
replaced connection dialing with no call, a rotated password file
authenticating the next connection, the credentials stream yielding only
changes, and `error_type` on failed series. Integration tests run against Valkey through
Compose (`CACHE_URL`). They cover round trip, TTL expiry, probe success,
unreachable and silent-server `Unavailable` within `command_timeout` plus
250 ms, outage and recovery through an in-test proxy (a hit again within
5 s), and TLS with a trusted CA and an untrusted CA. The service process test
points `APP__CACHE__DSN` at a closed local port with the local allow flags and
still becomes ready. A production environment with a plaintext DSN exits
non-zero before the listener.

The Go sibling template rejected a generic cache
(`specs/redis-valkey-cache-capability`). This template adopts that ceiling
because the request names the adopter and the hit/miss semantics. The ceiling
is one product and topology-specific client pack, an explicit endpoint, TLS,
and secret, bounded timeouts, sanitized telemetry, and native client exposure.
There is no generic `Cache<K, V>`, get-or-load, global TTL, serializer, or
locks.

This record does not claim a CI result, merge, publication, or deployment.
<!-- template:end cache:docs-cache-decisions -->
