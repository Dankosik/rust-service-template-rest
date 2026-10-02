# Cache decisions

<!-- template:begin cache:docs-cache-decisions -->
Stage 10.11 library decisions, recorded 2026-09-27 and extended through
2026-10-02. The owned supervisor and retryable password refresh replace the
manager-replacement, detached warm-up, and streaming-credential decisions.
[Guide](cache.md) owns adoption and observable behavior. This record retains
the accepted choices and their reopen conditions.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| `redis` 1.7.1, default features off, only `tokio-comp` and `tokio-rustls-comp` | `fred` 10.1.0 was last released and last pushed on 2025-02-27 (19 months) and had 43 open issues. `rustis` 0.26 has a small user base (30k recent downloads). `valkey-glide` has no crates.io Rust API; its core is used through FFI. | Pinned client and two features. TLS uses the process rustls provider and native roots, or a PEM at `root_ca_path`. `#insecure` fails because `tls-rustls-insecure` is not enabled. Reopen if `redis` fails this maintenance evidence, or a required command is absent. |
| One supervisor over canonical redis-rs multiplexed connections | `ConnectionManager` 1.7.1 retains detached reconnect work and offers no cancellation or connection-factory hook; caller response timeout leaves sent response slots in the driver. Canonical `Client::get_multiplexed_async_connection_with_config` handles instead abort the driver when their last clone drops. Pools are unnecessary for bytes-only commands. | Own generation identity, retirement, reconnect and cancellation in one private `connection.rs`; no pool or raw socket protocol. Reopen when a supported manager supplies bounded ownership, cancellation and credential recovery. Reopen a pool only for an accepted blocking or `WATCH`/`MULTI` requirement. |
| Distributed bytes-only client beside Moka | `moka` 0.12.16 permits independent process-local copies across multiple replicas; replica count alone does not require shared state. It cannot supply distributed rate limits or locks. | Use Moka when independent copies meet feature consistency needs; retain this profile for shared bytes. Reopen this profile if no deployment needs shared bytes. |
| Valkey as the tested server; Redis OSS-compatible for the command subset | Valkey is BSD-3-Clause, a Linux Foundation project, and a fork of Redis OSS 7.2.4. Redis 8 is AGPLv3 / RSALv2 / SSPLv1. `GET`, `SET` with `PX`, `DEL`, and `PING`, plus `HELLO 3` (with `AUTH` inside it), `CLIENT SETINFO`, `SELECT`, and direct `AUTH` for rotation, are identical in Redis OSS >= 7.2 and Valkey. | Standalone TCP only. Tested image `valkey/valkey:9.1.2-alpine@sha256:48332870af354a799964c0012ae1194a0bf2bf894eb508f945810596dc2d8d11`. Reopen Sentinel or Cluster only for an accepted topology requirement. |
| Compose, not testcontainers, for container tests | The repository already keeps Compose as the container environment and rejects a second runner beside it. The proof is the `valkey` service in `env/docker-compose.yml`. | Local and CI proof share that file. Reopen testcontainers only if Compose cannot host a required case. |
| No readiness gate | A gate would turn a cache outage into total unavailability and contradict degradation. | Startup still runs one probe inside 1 s, logs `cache_unavailable_at_startup` on failure, and continues. A service that requires the cache pushes `cache.probe()` into readiness. Never liveness. Reopen only if an accepted operation cannot degrade. |
| `cache.command_timeout` as configuration, default 100 ms, range 1 ms to 1 s, with `2 * cache.command_timeout <= http.request_timeout` | A constant cannot track the operator's request timeout. The rule leaves at least half of the request budget after one degraded call. It does not count a handler's calls: each sequential call can spend another `command_timeout`, so the feature budgets calls × `command_timeout` plus its own work and a response reserve. During an outage each call costs at most `command_timeout`. | One operator key. Connect stays the 1 s constant. Reopen a second timeout key only if connect and command must be tuned apart. |
| Connect, backoff, and TCP as constants | Each setup attempt has a 1 s envelope including password-file read. Existing `backon` 1.6.0 supplies 100 ms exponential backoff with factor 2 and jitter for six retries; cap every yielded sleep at 2 s because jitter is added after its `max_delay`. Pause 2 s after each failed chain and continue without traffic. TCP nodelay is on; keepalive is 30 s, interval 10 s, and three retries where supported; Linux `user_timeout` is 10 s. | No new operator keys or versions. At most one setup attempt or maintenance future exists. Reopen if a deployment cannot use these bounds. |
| Namespace name is the metric label and the key prefix (`{name}:{key}`) | Leaving keys unprefixed would let two features that share a server read each other's entries, and the metric label would not match the keyspace it measures. | A key written by another client must carry the same prefix to be shared. Reopen if a feature must read keys it does not own. |
| RESP3 always, whatever `protocol=` the DSN carries | The canonical connection uses `AsyncConnectionConfig::set_push_sender` for coalesced disconnect notification. Its callback retires only its own generation, so a late disconnect cannot remove a successor. | A server or proxy without `HELLO` (Redis before 6.0) remains unsupported. Reopen if an accepted endpoint cannot answer `HELLO 3`. |
| Retire a generation on command error or post-dispatch timeout, without replay | An established peer can accept commands and stop answering. Caller timeout alone does not remove sent slots. Retirement withdraws the matching generation immediately; old errors cannot affect a successor. Replacement dials wait only for the previous publication's 2 s spacing. Waiting for a connection does not retire setup. | `READONLY` and `OOM` retain conservative retirement; SET/DEL timeout remains effect-ambiguous. The public API and error vocabulary stay unchanged. Reopen if canonical last-clone drop no longer bounds retained slots. |
| `infra-cache` installs the aws-lc-rs rustls provider when the admitted address is TLS | redis 1.7.1 builds its TLS configuration with `rustls::ClientConfig::builder()` (`connection.rs`) and accepts no provider or ready configuration, so it needs the process default. The workspace compiles both `ring` and aws-lc-rs, so rustls cannot pick one and panics on the first dial. Installing it in the composition root instead was rejected: every binary that holds a `Cache` would have to repeat it, and forgetting it is a panic on a lazy connection, not a startup error. | A library sets a process default, idempotently, with the same provider every other crate in the workspace passes explicitly. Reopen when redis-rs accepts a provider or a `ClientConfig`. |
| One owned supervisor, with periodic bounded PING and final-owner cancellation | Detached warm-up and manager reconnect cannot supply the required lifetime. The supervisor independently advances setup and sends PING every 2 s with budget `min(command_timeout, 1 s)`; credential refresh has priority and maintenance never overlaps. Cache, namespaces and probes share an application owner; the supervisor holds no owner cycle. Final-owner drop withdraws state and cancels/aborts work. | At most 0.5 maintenance PING/s per healthy cache. With `B = max(command_timeout, 1 s)`, at most `1 + ceil(B / 2 s)` live generations; no absolute fan-in or byte bound. Connected probes have a 1 s ceiling; acquisition retains the caller's budget. Existing dependency-stage drop adds no wait or grace. Reopen if cancellation or driver ownership contradicts these bounds. |
| `cache.password_file` with supervisor-owned reads and direct retryable AUTH | The redis-rs 1.7.1 streaming provider exits after AUTH rejection and logs raw server text (`aio/multiplexed_connection.rs`); filtering logs cannot restore refresh. Every setup rereads the file. On an open connection the 5 s refresh tick shares a 1 s read/AUTH budget and compares against the last authenticated password. Only AUTH success accepts a changed value and logs `cache_password_reloaded`; rejected unchanged bytes remain pending for later ticks. | One file read per 5 s and at most one AUTH per refresh; no provider SDK or token minting. Unavailable files preserve a usable socket; new setup cannot use remembered credentials. Plain AUTH rejection may retain the socket; timeout/I/O/protocol failure retires it. Logs retain bounded classification only. Reopen the driver extension point when it supports owned, sanitized recovery after rejection. |
| `cache.client_cert_path` and `cache.client_key_path` through redis-rs's `ClientTlsConfig`, read once at admission | Valkey and Redis default to `tls-auth-clients yes`, so a self-hosted TLS server refuses a client without a certificate; without these keys the profile reached such a server only after the operator turned client authentication off. The driver takes the pair as PEM bytes and builds the rustls client configuration itself (`tls.rs`, `connection.rs`), so no template TLS code is added. redis 1.7.1 hands the pair to rustls only when it dials, where a key that does not belong to the certificate is `InvalidClientConfig` on every attempt; admission builds a rustls `CertifiedKey` from the same bytes so that case fails startup. Following a renewed certificate while running was rejected: connections retain the certificate bytes admitted at startup, so renewal would need certificate rereading and readmission as well as client rebuilding. A rolling restart on renewal remains the operation. | Two path keys, validated as a pair. A renewed certificate needs a restart; an expired one surfaces as `io` failures. Reopen reloading if an accepted deployment renews client certificates faster than it restarts. |
| Histogram only, with `error_type` on failed series | Hit, miss, and error counts are the `_count` series of `cache_operation_duration_seconds`. Labels are the closed sets `cache`, `operation` (`get`, `set`, `delete`), and `outcome` (`hit`, `miss`, `ok`, `error`, `timeout`, `cancelled`); `error` and `timeout` series add `error_type` (`timeout`, `io`, `auth`, `response`, `parse`, `other`). Without it the cause existed only on the span and a debug event, so at the production log level a refused password and a network outage were the same `error`; OpenTelemetry's `db.client.operation.duration` carries `error.type` for the same reason. | At most six more series per namespace and operation, registered on first use. No parallel counters. Reopen a counter only if an operator question cannot be answered from the histogram. |

Registry and maintenance evidence, fetched 2026-09-27: `redis` (redis-rs) 1.7.1, released 2026-09-25, BSD-3-Clause, MSRV 1.88, repository pushed 2026-09-25, ~26.6M recent downloads. `fred` 10.1.0, last release and last push 2025-02-27, 43 open issues. `deadpool-redis` 0.23.1, 2026-08-26, depending on `redis` ^1.6. `bb8-redis` 0.26.0, 2025-12-09, depending on `redis` ^1. `rustis` 0.26, 30k recent downloads. `valkey-glide` has no crates.io Rust API. `moka` 0.12.16 is already in the workspace. Valkey is BSD-3-Clause. Redis 8 is AGPLv3 / RSALv2 / SSPLv1. redis-rs MSRV 1.88 fits workspace Rust 1.99. These figures do not describe the resolved lock.

## Resolved dependency graph

`infra-cache` retains redis 1.7.1 with `tokio-comp` and `tokio-rustls-comp`.
The supervisor reuses the already declared and locked `backon` 1.6.0 (`std`),
`tokio-util` (`rt`), and Tokio synchronization/macros. The obsolete
`connection-manager`, `token-based-authentication`, and direct `futures-util`
stream edges are removed. `Cargo.lock` owns the resulting transitive package
set; no dependency version upgrade is part of this change.

`socket2` 0.6.5 remains a direct edge because redis does not re-export
`TcpKeepalive`, which sets keepalive retries. `xxhash-rust` uses BSL-1.0
(Boost Software License, OSI approved), allowed by `deny.toml` within the
cache marker. `CACHE=none` projects away the cache's packages and edges.

## Supported extension points

A later accepted rate-limit or lock capability can add its provider operation
beside `get`, `set`, and `delete`, sharing bounded connections and observation.
Feature behavior remains provider-free; composition or a service adapter calls
`infra-cache` and translates feature-defined requests and results. No raw
connection or generic cache interface is exposed to a feature. These additional
capabilities are not in this profile. Readiness remains an opt-in through
`cache.probe()` in `crates/service/src/bootstrap/mod.rs`. Sentinel, Cluster,
and pools stay closed without an accepted topology or command requirement.

## Ownership and proving surfaces

`infra-cache` owns admission, namespace operations, the probe and sanitized
observation. Its private `connection.rs` owns the supervisor, generation
fencing, independent recovery and final-owner cancellation; `credentials.rs`
owns password-file admission and bounded reads. `service-config` owns the
`[cache]` section. Bootstrap owns construction, the bounded startup check,
optional readiness registration and shutdown drop. Features own keys,
serialization, TTL, invalidation and fallback; composition/adapters invoke the
provider and translate their results. HTTP handlers own status projection.

The proving surfaces remain crate unit/protocol tests, Compose Valkey tests,
crate doctests, and service process tests. Admission/TLS, namespace, metrics,
round-trip and expiry coverage remains applicable. Reliability coverage must
discriminate a silent established peer, no replay of timed-out writes,
retirement of cancelled sent slots, late errors from old generations,
final-owner cancellation and legitimate retained handles, rejected unchanged
password retry, and sanitized rendered dependency logs. Public long command
timeouts are part of resource accounting; service validation alone is not an
assumption for direct callers. Service tests retain the degrading startup and
existing teardown boundary. Final validation reports what actually ran; this
decision record does not certify those results.

The Go sibling template rejected a generic cache
(`specs/redis-valkey-cache-capability`). This template adopts that ceiling
because the request names the adopter and the hit/miss semantics. The ceiling
is one product and topology-specific client pack, an explicit endpoint, TLS,
and secret, bounded timeouts, sanitized telemetry, and native client exposure.
There is no generic `Cache<K, V>`, get-or-load, global TTL, serializer, or
locks.

This record does not claim a CI result, merge, publication, or deployment.
<!-- template:end cache:docs-cache-decisions -->
