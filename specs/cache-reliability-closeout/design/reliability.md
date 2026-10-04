# Cache reliability design

Status: ready. Baseline `67be869`, 2026-10-02. Behavior authority:
[Specification](../spec.md), with [Definition review](../definition-review.md).
This design replaces the cache's manager wrapper, not redis-rs's transport,
TLS, RESP, multiplexing, or public bytes-only API.

## Selection and evidence

Use one cache-owned supervisor over redis 1.7.1's canonical
`Client::get_multiplexed_async_connection_with_config`. Keep one current
connection, fence retirement by connection-generation identity, and own
reconnect and password refresh in that supervisor. There is no pool, raw
socket implementation, new configuration key, or command retry.

The resolved registry source is authoritative:
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/redis-1.7.1/src/`.
Versioned public API pages are
[MultiplexedConnection](https://docs.rs/redis/1.7.1/redis/aio/struct.MultiplexedConnection.html)
and [ConnectionManagerConfig](https://docs.rs/redis/1.7.1/redis/aio/struct.ConnectionManagerConfig.html).
The web reader could not retrieve those pages; decisions use the installed
version's source, including its public API documentation.

| Surviving choice | Decisive evidence and disposition | Cost and reopen condition |
| --- | --- | --- |
| Keep `ConnectionManager`, tune response timeout/concurrency and swap generations | Rejected together for R1/R2: `aio/multiplexed_connection.rs:459-515` times out the receiving future without removing the sent response slot. `aio/connection_manager.rs:613-663` spawns a detached reconnect future retaining `Internals`; its public configuration exposes no cancellation, close, or connection-factory hook. Finite caller timeouts plus replacement do not cancel that work. Concurrency limits bound active waiters, not cancelled sent slots. | A manager upgrade with supported cancellation and the needed credential semantics could remove the supervisor; changing version is outside this task. |
| Retain driver streaming credentials and filter its logs | Rejected for R3/R4: `aio/multiplexed_connection.rs:714-745` exits on AUTH rejection and logs raw server error text. A log filter cannot restore the subscription. A replacement layer would still need its own credential state and recovery owner. | Avoids adding telemetry-wide suppression or keeping two recovery owners. Reopen when the driver's extension point survives rejection and provides sanitized, owned failure handling. |
| Canonical multiplexed connection plus one supervisor | Selected. `client.rs:547-559` installs the driver's shared task handle; `aio/multiplexed_connection.rs:552-562` documents that dropping all canonical connection clones aborts the driver even with unanswered commands. `AsyncConnectionConfig::set_push_sender` supports a coalesced RESP3 disconnect signal. `Client::get_connection_info`, `ConnectionInfo::set_redis_settings`, and `RedisConnectionInfo::set_password` preserve the admitted address/TLS/TCP settings when supplying a freshly read password. | Own a small connection/credential state machine and one periodic PING per healthy cache, at most once per 2 s. Retire this custom policy when a maintained supported manager meets all of R1-R4. |

`backon` 1.6.0 is already declared and locked; reuse its exponential builder
for the existing 100 ms, factor 2, six-retry schedule. Its resolved
`src/backoff/exponential.rs:202-252` adds jitter *after* `max_delay`, so cap
each yielded delay at the existing 2 s constant. The old documentation's
2 s ceiling was not a bound on the jittered sleep. No retry crate or version
is added. The selected driver and backoff versions reuse the repository's
existing library selection; broader pool/client alternatives do not survive
the fixed topology, version and no-new-capability constraints.

## Ownership and state

`Cache`, `CacheNamespace` and `CacheProbe` retain the same application-owner
`Arc`. That owner retains the supervisor join/abort handle, a cancellation
token and shared connection state. The supervisor owns only shared state,
admitted connection inputs and its cancellation token; it never owns that
application-owner `Arc`. Driver callbacks retain weak shared state or a small
generation signal, never a `Cache` or a strong cycle through the connection.

Shared state contains either one published generation or no connection, plus
closed state and coalesced change notification. A generation has an identity,
the canonical multiplexed handle and retirement notification. Short synchronous
locking covers snapshot/publication/retirement only, never network I/O. An
operation snapshots both handle and identity; every failure retires only the
matching current identity. A late old error, timeout or disconnect callback
cannot clear or replace a successor. A caller admitted just before retirement
may already have sent its command; retirement does not retract that effect.

The final application owner closes shared state, retires the current generation,
cancels and aborts the supervisor. Every supervisor wait (file read, setup,
AUTH, PING and sleep) observes owner and generation cancellation as applicable;
publication and new work check closed state. The supervisor's exit cleanup
also withdraws its current connection and wakes waiters on unexpected exit.
Dropping a discarded join handle is never the cancellation mechanism.

The existing service dependency-stage drop remains the lifecycle integration:
it requests cancellation synchronously; runtime scheduling completes destruction.
There is no new async close, grace period, or service task. The same Drop path
covers failed/interrupted startup. A retained namespace or probe legitimately
keeps the cache alive. `tokio::fs` may finish an already dispatched blocking OS
read after its Rust future is dropped; it owns no manager/connection and cannot
publish a credential, reconnect or continue the refresh loop after cancellation.

## Material flows

1. **Admission and connection.** Preserve all current DSN, password-file,
   username, TLS, certificate and runtime admission. Construction performs no
   network I/O and starts one supervisor immediately. Each setup attempt has
   one 1 s envelope covering current-file read, client construction and
   DNS/TCP/TLS/HELLO setup. When a password file is configured, derive a client
   from a clone of the already admitted `ConnectionInfo` with the freshly read
   password and existing username; its TLS address carries the admitted TLS
   configuration. Do not install `StreamingCredentialsProvider`. An unavailable
   file fails that attempt with a bounded classification; never use a remembered
   password for a new connection. Successful setup publishes the generation
   and its authenticated password together. Check cancellation before publishing.

2. **Startup, setup failure and disconnect.** Callers wait for publication under
   their own budget; the supervisor advances independently of demand. All setup
   errors, including non-I/O AUTH/HELLO errors, retry with the existing jittered
   schedule. Seven attempts exhaust one chain; after a 2 s pause start another
   chain while the owner lives. This replaces the finite detached warm-up and
   avoids a stored terminal setup error. RESP3 disconnection uses a callback
   that coalesces notification and retires its own generation; no unbounded push
   queue is needed. Replacement connection attempts begin no earlier than 2 s
   after the preceding generation's publication, with no restriction on the
   first generation. An old generation is withdrawn immediately, even when the
   next dial must wait for that spacing. Thus successive successful publications
   are at least 2 s apart. Failed setup attempts use the backoff schedule instead.

3. **GET/SET/DEL and probe.** One absolute `command_timeout` covers connection
   wait and exactly one dispatched command. Keep the existing result, metrics
   and error vocabulary. On timeout after dispatch, or a Redis command error,
   retire the captured generation and return `Unavailable`; do not resubmit the
   command. A timeout while only waiting for an initial/replacement connection
   does not cancel healthy progress on setup. Keep the current conservative
   replacement after non-I/O command errors, including READONLY/OOM. Operations
   also stop on their generation's retirement. Probes share the same acquisition
   and generation fence: connection wait keeps its external startup/readiness
   budget, while a connected PING gets a 1 s internal response ceiling and
   retirement cancellation. This is an explicit refinement of the old claim
   that a probe has no internal timeout; optional readiness and the 1 s degrading
   startup envelope stay unchanged. Probe errors contain classification only.

4. **Cancelled command/probe and silent established peer.** On each live
   generation the supervisor sends one PING every 2 s, with response budget
   `min(command_timeout, 1 s)`; no overlapping maintenance commands. Its timeout
   or error retires that generation. This independently drains or retires sent
   slots whose caller was cancelled before seeing a timeout. Cancelling a caller
   alone does not retire a healthy connection. The PING task is the supervisor's
   current future, not a spawned task. Credential refresh has priority when due;
   delayed ticks never burst or accumulate. All deadlines use Tokio time.

5. **Password refresh.** Keep the 5 s refresh cadence. One refresh (read and,
   when needed, direct AUTH on the current generation) has a shared 1 s envelope.
   Track the last *authenticated* password separately from the latest readable
   value. A changed value triggers AUTH; only successful AUTH updates acceptance
   and logs `cache_password_reloaded`. Server rejection emits a sanitized bounded
   error and leaves that value pending: a later tick retries it even when the
   file bytes are unchanged. Reading an accepted unchanged value needs no AUTH.
   A later unreadable/empty file leaves the usable authenticated connection and
   last accepted value intact, warns once per outage, and does not end refresh.
   I/O failure, timeout, or unusable protocol during AUTH retires the generation;
   a plain AUTH rejection may retain the previous authenticated socket. The next
   connection always rereads the file. A later valid credential or later server
   acceptance therefore recovers without traffic or restart. No AUTH success or
   failure from a retired identity affects its successor.

6. **Diagnostics and teardown.** The streaming-provider branch that logs raw
   server errors is unreachable. Setup, direct AUTH, PING and command failures
   cross only `observe::error_type` or existing sanitized admission errors;
   neither raw `RedisError` nor credential-bearing inputs are formatted. Retain
   redacted Debug for owner/state/credentials and never log file content, paths,
   keys, values or DSNs. Final-owner drop cancels all of flows 1-5; no retry or
   useful credential work is admitted afterward.

## Bounds and accepted costs

Let `C` be the supplied command timeout, `T = 1 s` the existing connect budget,
`R = 2 s` the existing replacement/backoff ceiling, `F = 5 s` the refresh cadence,
`W = min(C,T)` the maintenance PING budget, and `B = max(C,T)` the conservative
old-handle lifetime. These are finite async scheduling bounds, not hard realtime
claims under a stopped executor. Public `CacheOptions` is not assumed to have
passed service configuration validation: existing direct callers use `C = 30 s`.

| Property | Bound and derivation |
| --- | --- |
| User operation | At most C including acquisition; zero command replay. SET/DEL timeout remains effect-ambiguous. |
| Probe | Acquisition uses its caller's budget; a connected exchange lasts at most T and also ends on retirement. Cancelled probes retain no operation handle. |
| Replacement eligibility after observed established timeout | Withdraw immediately; next dial is eligible within R of failure because only the preceding publication's R spacing can delay it. |
| Abandoned sent-slot lifetime | A periodic PING observes a silent generation within R + T + W: next tick plus at most one in-progress credential refresh plus the PING. Retirement releases the state and maintenance handles; outstanding operations release within B, normally sooner on retirement notification. Canonical last-clone drop aborts the driver and its response slots. No slots survive indefinitely across generations. |
| Live connection generations | At most `1 + ceil(B / R)`: one current or connecting generation plus retirees whose handles may last B, with successful publications spaced R. This is 2 for service-validated C <= 1 s, and 16 for C = 30 s. Each cancelled operation drops its handle immediately; the conservative formula does not rely on that optimization. At most one setup attempt, one supervisor and one maintenance future exist. |
| Setup chain and unavailable endpoint | At most `7*T + 6*R = 19 s` per chain, then R before restarting. Actual capped jitter sleeps are smaller. Persistent failure retains one bounded attempt/sleep, never one task per chain. Reachability plus a readable accepted credential permits success within `T + R + T = 4 s`, allowing an already failing attempt, its maximum sleep and a successful attempt. |
| Rotation after the file/server becomes usable | On a retained socket: `F + W + T <= 7 s` for next refresh, one in-progress PING and read/AUTH. If it retires instead, add at most `T + R + T = 4 s` for recovery; the conservative composed ceiling is 11 s, assuming that subsequent attempt reaches a server accepting the supplied credential. Unchanged rejected bytes remain retryable. |
| Final-owner release | Immediate cancellation/abort request and removal of published handles; scheduled destruction releases the supervisor and driver. No 20 s warm-up or retry chain is awaited; service teardown adds no wait or grace. |

There is no absolute byte or request-count promise for arbitrary caller fan-in:
the driver keeps its existing queue and multiplexing policy. The guarantee is
bounded generation count and retention time, so obsolete response slots cannot
accumulate across cycles. A healthy cache pays at most 0.5 extra PING/s and,
with a password file, one file read every 5 s. Rejected pending credentials pay
at most one AUTH per refresh; disconnected caches pay the bounded retry chain.

## Placement and dependency decision

Use the existing crate boundary. A private `connection.rs` owns the one new
cohesive lifecycle; its removal would mix admission/API and owner cancellation,
recovery, generation fencing and background scheduling back into `lib.rs`.
The code/ownership placement has no surviving multi-crate or interface fork;
Technical Design owner self-review supplies the ownership disposition.

| Responsibility | Owner and exact action | Cleanup, proof and reopen |
| --- | --- | --- |
| R1/R2 connection generations and supervisor | Add private `crates/infra-cache/src/connection.rs`; move Link lifecycle out of `lib.rs`. Keep only crate-private operations for acquisition, retirement and owned start/drop. It may use redis, backon, Tokio, tokio-util and the existing observation/credential owners; no service or feature dependency. | Delete manager replacement, detached `warm_up`, `WARM_UP_TIMEOUT`, and old retry owner. `infra-cache` owns protocol/resource proof. Reopen this design if supported clone-drop semantics or cancellation accounting fails. |
| Public admission, operation observation and probe | `crates/infra-cache/src/lib.rs` retains public Cache/CacheOptions/Namespace/Probe and admission. Delegate one-command connection work to the private lifecycle; preserve get/set/delete signatures. | Existing admission/TLS/doctest/metrics tests remain proof; no new cache trait or application interface. |
| R3 credential admission and refresh state | `crates/infra-cache/src/credentials.rs` retains PasswordFile admission/newline/username policy and adds crate-private bounded reads plus accepted-versus-pending state used by the supervisor. Secret fields have redacted Debug. | Remove StreamingCredentialsProvider/Watch stream ownership and tests tied solely to that superseded mechanism. Reuse behavioral file coverage; add rejection-recovery falsifiers at the protocol boundary. |
| R4 sanitized diagnostics | Existing `crates/infra-cache/src/observe.rs` remains the error-classification owner; change only if a shared bounded lifecycle event needs it. The main change is removal of the raw logging driver path. | Existing log bridge remains untouched. Existing `crates/infra-cache/src/tests.rs` hosts negative rendered-log proof through that bridge and fake server errors. |
| Dependency declarations | `crates/infra-cache/Cargo.toml`: remove redis `connection-manager` and `token-based-authentication`; retain tokio-comp/tokio-rustls-comp. Add already-declared backon with std, tokio-util with rt, and explicit Tokio sync/macros needed by the supervisor. Remove futures-util if its only owner was the credentials stream. | Refresh Cargo.lock normally for direct edges/now-unneeded packages; no version upgrade, manual lock editing or new workspace version. Match profile projection through existing initializer gates. Manifest changes select one workspace build/test at final validation. |
| Regression proof | Existing `crates/infra-cache/src/tests.rs`, credential unit module, `crates/infra-cache/tests/valkey.rs`, and `crates/service/tests` process suite remain the proving surfaces. | Executor chooses the smallest added cases; existing process drop integration needs no source change unless the selected mechanism cannot satisfy its claimed lifetime. That evidence reopens this design before adding async close. |
| R5 adoption/lifecycle guidance | Update `docs/cache.md`, `docs/cache-decisions.md`, cache sections of `docs/architecture/{boundaries,integration,runtime-lifecycle}.md`, `docs/backend-library-selection.md`, and crate-level example prose in `lib.rs`; fix other live cache prose only where it names a superseded mechanism. | Explain multiple replicas may each use Moka with independent copies. Features own behavior; composition/service adapters invoke infra-cache and translate feature-defined requests/results. Label example cache calls as adapter/composition code, without inventing a feature, trait or generic cache interface. |

The responsibility table is also the inverse file map: each named Rust source
has one row defining its visibility, call-path/lifecycle/error role, allowed
dependencies and forbidden feature/service ownership. Test files own behavior
proof only. All output is manual source; there is no generated API contract or
transport change. Service bootstrap/config/health source remain consumers of
their existing public contract.

## Falsifiers and movement

Implementation must reject: an established silent peer surviving repeated
retirement; a timed-out write replay; cancelled command/probe slots living
forever; a stale generation destroying a working successor; last-owner drop
waiting for warm-up; retained namespace/probe losing ownership early; unchanged
rejected credentials never retried; file-read success recorded as AUTH success;
and server markers/secrets escaping through the real log bridge. Include public
long-timeout accounting when proving retirement, rather than assuming C <= 1 s.
Existing fake-server/socket/task observations can discriminate these mechanisms;
Valkey, admission/TLS and service process proof retain their existing owners.
This is feasible-proof guidance, not a prescribed test matrix or new runner.

One integrated implementation unit is feasible: mechanism, credentials and docs
must land together. Final validation is the repository's manifest/changed-surface
route, once after assembly; CI owns its Valkey/profile gates. No build, runtime
repair, heavy validation, CI result, merge or deployment is claimed by this
Technical Design phase. Fresh Technical Design review is required before
Planning. Reopen this design for contradicted mechanism/bounds, Research for
changed driver evidence, or Specification only for changed accepted behavior.
