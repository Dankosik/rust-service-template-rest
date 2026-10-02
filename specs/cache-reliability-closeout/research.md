# Cache closeout evidence

Valid as of 2026-10-02 at `67be869`; inspected against redis-rs 1.7.1 registry
source. These findings establish behavior gaps, not a required mechanism.
Refresh on dependency, command path, credential path, or lifecycle changes.

| Claim and disposition | Primary evidence and limits |
| --- | --- |
| Established connections can remain stuck after caller timeout. Confirmed. | [`CacheNamespace::run`](../../crates/infra-cache/src/lib.rs) returns `Unavailable` on elapsed without calling recovery; `manager_config` sets `response_timeout(None)`. redis 1.7.1 `src/aio/multiplexed_connection.rs` appends sent commands to `in_flight` at line 367 and consumes entries on server response at line 217. Dropping a caller can shed an unsent message (line 344), but does not retract an already sent response slot. |
| The stall reproduces after successful setup, not merely during handshake. Supporting measured evidence, supplied by the parent and read back here. | Scratch `result.txt`: `post_handshake_stall: timeouts=150, commands_received=150, tcp_connections_before=1, tcp_connections_after=1`. Source/result location: `/var/folders/9r/ft1t72w13r765bpf61v9mly00000gn/T/infra-cache-readonly-review-9sodnuwd/`. It proves no connection replacement in that experiment. RSS was not measured; no numerical memory-growth or throughput claim follows. |
| Warm-up work retains a manager beyond the cache owner's lifetime. Confirmed. | [`Link::replace_if_stuck`, `Cache::connect_lazy`, `warm_up`](../../crates/infra-cache/src/lib.rs) spawn detached tasks holding manager clones for up to 20 s. [`Dependencies::close`](../../crates/service/src/bootstrap/shutdown.rs) only drops the cache. A finite 20 s lifetime is counter-evidence to an unbounded-task claim, but does not establish owner-driven cancellation. |
| A rejected live re-authentication ends that connection's password subscription. Confirmed. | redis 1.7.1 `src/aio/multiplexed_connection.rs:714-745` returns from the credentials task on either dropped-connection or other AUTH error. [`PasswordFile`](../../crates/infra-cache/src/credentials.rs) keeps producing changes; that cannot help once its consumer has exited. Existing stream-change and next-connection tests do not establish rejection-then-recovery on an open connection. |
| Raw server error text can cross the claimed sanitized telemetry boundary. Confirmed source path; no production leak claim. | The same redis re-auth branch logs `{err}` through `log` at lines 729 and 733. [`logging.rs`](../../crates/infra-telemetry/src/logging.rs) documents and installs the `tracing-log` bridge. [`Cache observability`](../../docs/cache.md#observability) promises no raw server text, keys, values, or DSN in logs. |
| Adoption guidance contradicts both useful Moka semantics and repository dependency policy. Confirmed. | [`Cache`](../../docs/cache.md#when-process-local-moka-is-enough) unnecessarily requires one replica for independent process-local caches. [`Component boundaries`](../../docs/architecture/boundaries.md#dependency-direction) says `a feature -> infra-cache`; [`Repository architecture`](../../docs/repo-architecture.md#global-invariants) forbids feature dependencies on provider crates. There is no existing feature crate requiring migration. |

Registry source root:
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/redis-1.7.1/`.
Repository source and the resolved registry code are the decision-critical
authority; the scratch experiment is corroboration and is not a durable test
or acceptance receipt. Technical Design must inspect supported extension
points before choosing how to repair these gaps. Tests remain executor-owned.
