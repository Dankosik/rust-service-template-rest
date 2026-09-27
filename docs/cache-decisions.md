# Cache decisions

<!-- template:begin cache:docs-cache-decisions -->
Stage 10.11 library and lifecycle decisions, recorded 2026-09-27.
[Guide](cache.md) owns adoption and observable behavior. This record retains
the accepted choices and their reopen conditions.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| `redis` 1.7.1, default features off, only `tokio-comp`, `connection-manager`, and `tokio-rustls-comp` | `fred` 10.1.0 was last released and last pushed on 2025-02-27 (19 months) and had 43 open issues. `rustis` 0.26 has a small user base (30k recent downloads). `valkey-glide` has no crates.io Rust API; its core is used through FFI. | Pinned client and three features. TLS uses the process rustls provider and native roots, or a PEM at `root_ca_path`. `#insecure` fails because `tls-rustls-insecure` is not enabled. Reopen if `redis` fails this maintenance evidence, or a required command is absent. |
| `ConnectionManager`: one multiplexed lazy connection | `deadpool-redis` 0.23.1 (2026-08-26, `redis` ^1.6) and `bb8-redis` 0.26.0 (2025-12-09, `redis` ^1) are not needed. The multiplexed connection already pipelines concurrent requests. Pools pay off for blocking commands, `WATCH`/`MULTI`, or pub/sub, which this cache does not use. | No pool crate. Reopen with `deadpool-redis`, which is more current than `bb8-redis`, if a feature needs `WATCH`, `MULTI`, or a blocking command. |
| Distributed bytes-only client, not moka alone | `moka` 0.12.16 is already the process-local cache. It does not share state across replicas and cannot back distributed rate limits or locks. | A second mechanism beside `moka`. `moka` stays the per-process choice. Reopen this profile only if no deployment needs shared bytes. |
| Valkey as the tested server; Redis OSS-compatible for the command subset | Valkey is BSD-3-Clause, a Linux Foundation project, and a fork of Redis OSS 7.2.4. Redis 8 is AGPLv3 / RSALv2 / SSPLv1. `GET`, `SET` with `PX`, `DEL`, and `PING`, plus `HELLO`, `AUTH`, and `SELECT` by the client, are identical in Redis OSS >= 7.2 and Valkey. | Standalone TCP only. Tested image `valkey/valkey:9.1.2-alpine@sha256:48332870af354a799964c0012ae1194a0bf2bf894eb508f945810596dc2d8d11`. Reopen Sentinel or Cluster only for an accepted topology requirement. |
| Compose, not testcontainers, for container tests | The repository already keeps Compose as the container environment and rejects a second runner beside it. The proof is the `valkey` service in `env/docker-compose.yml`. | Local and CI proof share that file. Reopen testcontainers only if Compose cannot host a required case. |
| No readiness gate | A gate would turn a cache outage into total unavailability and contradict degradation. | Startup still runs one probe inside 1 s, logs `cache_unavailable_at_startup` on failure, and continues. A service that requires the cache pushes `cache.probe()` into readiness. Never liveness. Reopen only if an accepted operation cannot degrade. |
| `cache.command_timeout` as configuration, default 100 ms, range 1 ms to 1 s, with `2 * cache.command_timeout <= http.request_timeout` | A constant cannot track the operator's request timeout. The rule leaves at least half of the request budget for the source of truth. During an outage each call costs at most `command_timeout`. | One operator key. Connect stays the 1 s constant. Reopen a second timeout key only if connect and command must be tuned apart. |
| Connect, backoff, and TCP as constants | `connection_timeout` is 1 s. Backoff uses `min_delay` 100 ms, `exponent_base` 2, `max_delay` 2 s, and `number_of_retries` 6. TCP nodelay is on. Keepalive is 30 s, interval 10 s, and 3 retries where supported. Linux `user_timeout` is 10 s so a half-open connection is detected and reconnected. | Not operator keys. Reopen if a deployment cannot use these bounds. |
| Histogram only | Hit, miss, and error counts are the `_count` series of `cache_operation_duration_seconds`. Labels are the closed sets `cache`, `operation` (`get`, `set`, `delete`), and `outcome` (`hit`, `miss`, `ok`, `error`, `timeout`, `cancelled`). | No parallel counters. Reopen a counter only if an operator question cannot be answered from the histogram. |

Registry and maintenance evidence, fetched 2026-09-27: `redis` (redis-rs) 1.7.1, released 2026-09-25, BSD-3-Clause, MSRV 1.88, repository pushed 2026-09-25, ~26.6M recent downloads. `fred` 10.1.0, last release and last push 2025-02-27, 43 open issues. `deadpool-redis` 0.23.1, 2026-08-26, depending on `redis` ^1.6. `bb8-redis` 0.26.0, 2025-12-09, depending on `redis` ^1. `rustis` 0.26, 30k recent downloads. `valkey-glide` has no crates.io Rust API. `moka` 0.12.16 is already in the workspace. Valkey is BSD-3-Clause. Redis 8 is AGPLv3 / RSALv2 / SSPLv1. redis-rs MSRV 1.88 fits workspace Rust 1.98. These figures do not describe the resolved lock.

## Resolved dependency graph

TODO(lead): packages added to Cargo.lock

## Supported extension points

`Cache::connection` returns `redis::aio::ConnectionManager` so a later feature
can build rate limits or locks without a second client. Those capabilities are
not in this profile. Readiness is an opt-in: composition pushes `cache.probe()`
into the probes in `crates/service/src/bootstrap/mod.rs`. Sentinel, Cluster,
and a connection pool stay closed. Reopen a pool with `deadpool-redis` when a
feature needs `WATCH`, `MULTI`, or a blocking command. Reopen Sentinel or
Cluster only with an accepted topology requirement.

## Ownership and proving surfaces

`infra-cache` owns admission, the lazy client, namespace operations, the
probe, and sanitized observation. `service-config` owns the `[cache]` section.
Bootstrap owns connect, the bounded startup check, optional readiness
registration, and the shutdown drop. The feature owns keys, serialization,
TTL, invalidation, and any map from `Unavailable` to HTTP 503.

Unit tests cover the admission matrix (plaintext refused or allowed, missing
password refused or allowed, `#insecure` refused, unix socket refused, CA with
plaintext refused, missing CA file, garbage CA), `CacheError` Display and
Debug never containing the password, namespace name validation, `error.type`
mapping, and observation labels. Integration tests run against Valkey through
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
