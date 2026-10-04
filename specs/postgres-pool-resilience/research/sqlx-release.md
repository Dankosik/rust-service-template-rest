# SQLx release and acquisition evidence

Historical supporting Technical Design research, 2026-10-04. Source hashes and
the probe remain mechanism evidence. Facade, hidden application API use and
async-stream/TaskTracker selections below belong to the superseded option;
current design/design.md and design/dependency-custody.md replace them. This
is not acceptance of an implemented adapter. Repository base:
`67be869acea112af271ec8ba621cbc50ae9d36b7`; worktree lock SHA256:
`9273481496362e1684634bf50e6e6c2df2354404ccaa6ca3621dc30ff58f9a1f`.

## Resolved authority

The lock resolves SQLx, sqlx-core and sqlx-postgres to 0.9.0. The
[registry API](https://crates.io/api/v1/crates/sqlx), read on 2026-10-04,
reported `max_stable_version = max_version = newest_version = 0.9.0`, with
the crate updated on 2026-05-21. No upgrade is selected.
[Upstream](https://github.com/launchbadge/sqlx) was unarchived, with its most
recent push reported as 2026-10-04. Exact behavior below comes from the
installed registry source, not latest-main assumptions. Its root is
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

| Relative source | SHA256 |
| --- | --- |
| `sqlx-core-0.9.0/src/pool/connection.rs` | `94269a532ef60aaa31321e43af17cba2424f919a36501d862328ed05958a08de` |
| `sqlx-core-0.9.0/src/pool/inner.rs` | `cba56628f259270dccb2f4a0d7a351dcff34d9239f6e6e1aee187ebe0e39c539` |
| `sqlx-core-0.9.0/src/pool/options.rs` | `924cd57a062328b2e7c6ac55242eabb1706893d80ce1e8cb02fe643a592f4f83` |
| `sqlx-core-0.9.0/src/pool/executor.rs` | `dbd98cd32d60361c515ae1311d461f5e09fd41d0033f37a65ae1417484c1d4be` |
| `sqlx-postgres-0.9.0/src/connection/mod.rs` | `efeafd9bc07c0bd20e0d2c2312bd2db45039c9d139aa754b1ff6574dd280de72` |
| `sqlx-postgres-0.9.0/src/connection/executor.rs` | `721ead17743d912c625a56b5728669d8111c750797b6fee24e234226a5b6fbe6` |
| `sqlx-postgres-0.9.0/src/transaction.rs` | `3f326f8d6bf840a112cb98e465390d9add493b88f87e8a56006d91396bc0ce00` |

## Decision-changing behavior

1. `PgConnection::ping` queues a protocol `Sync` and waits for readiness.
   `write_sync` increments the pending ReadyForQuery count;
   `wait_until_ready` flushes the output and consumes every pending response.
   A second ping starts another real round trip even after a first ping
   succeeded. `Connection::flush` calls the same readiness wait; it is not a
   local-buffer-only operation. `should_flush` tests only the write buffer and
   cannot detect an unanswered query already written to the socket.
   Sources: [PostgreSQL connection](https://docs.rs/sqlx-postgres/0.9.0/src/sqlx_postgres/connection/mod.rs.html)
   at lines 94, 180, 238; `connection/executor.rs:150`.
2. A raw `PoolConnection` drop spawns `return_to_pool`. Its `Floating`
   value holds the connection and `DecrementSizeGuard`, therefore capacity is
   occupied until cleanup finishes. Cleanup closes expired/closed-pool
   connections before callbacks; otherwise it runs `after_release`, then an
   unconditional unbounded ping. A successful callback cannot bound the
   later ping. `before_acquire` cannot see a slot still held by cleanup.
   Sources: [pool connection](https://docs.rs/sqlx-core/0.9.0/src/sqlx_core/pool/connection.rs.html)
   at lines 134, 199, 275; [pool inner](https://docs.rs/sqlx-core/0.9.0/src/sqlx_core/pool/inner.rs.html)
   at lines 210 and 614.
3. `PoolConnection::return_to_pool` is a public **doc-hidden** method, not a
   documented stable extension contract. It synchronously takes the live
   connection into an owned, `Send + 'static` future before returning. Dropping
   that future drops the raw socket and then its size guard, freeing the
   permit without protocol acknowledgement. Timing out the whole future also
   bounds callbacks, final ping, and the close/lifetime branches. This is the
   selected narrow dependency on the resolved driver; an SQLx upgrade must
   recheck it. The existing default `min_connections = 0` avoids refill work
   after the permit is released.
4. `close_on_drop` uses its own five-second timeout. It is bounded, but exceeds
   R1's three-second assumption and does not help raw direct-pool acquisition
   diagnostics. `detach` frees capacity while returning a still-live socket;
   its contract explicitly permits exceeding the pool maximum. `close_hard`
   consumes the raw connection and cannot be called from a borrowed callback.
   `close` is graceful socket I/O; it can block. These are not selected.
5. Native acquire owns a semaphore permit or a floating connection under one
   total acquire timeout. Cancellation while waiting, dialing, or idle-checking
   drops that ownership. There is no await between returning an acquired raw
   handle and wrapping it in the adapter. Native acquire emits slow/success
   tracing only after `Ok`; it emits no acquisition-timeout event and has no
   pool identity field. Its prose incorrectly mentions PoolClosed for timeout;
   the implementation returns PoolTimedOut. Source: `pool/inner.rs:245–325`
   and `pool/mod.rs:334–362`.
6. PostgreSQL transaction depth increments after BEGIN acknowledgement.
   Cancelling BEGIN can therefore leave an open server transaction with
   driver depth zero, and the ordinary drop queues no rollback. Ping success
   does not establish that this connection is outside a transaction. Keep
   the template's pending-BEGIN guard, but discard by dropping the unpolled
   return future. For later transaction cancellation, SQLx queues rollback
   before the checkout returns; bounded cleanup may safely drain it and reuse
   the connection. It supplies no evidence of an uncertain COMMIT outcome.
   Sources: `sqlx-postgres/src/transaction.rs:16`; existing
   [transaction boundary](../../../crates/infra-postgres/src/transaction.rs).

## Alternatives and supporting dependencies

Supported hooks/configuration were considered first. Their exact missing
surface is an all-return timeout and an all-acquisition outcome hook. A larger
pool, statement timeout, connection lifetime, TCP keepalive, timeout around
the caller, or bounded callback ping does not fill both gaps. Always closing
healthy connections would lose reuse. A SQLx fork would carry a patch/release
owner; driver replacement is outside the accepted requirement.

Registry alternatives read on 2026-10-04: `deadpool-postgres` 0.14.2
(2026-08-26), `bb8-postgres` 0.9.0 (2024-12-09). They wrap tokio-postgres,
not the retained SQLx Executor/transaction/migration API, so neither is a
same-boundary replacement. No further competing-driver experiment is useful.

The adapter facade needs to retain lazy, cancellable SQLx row streaming while
owning its checkout. Select [async-stream 0.3.6](https://docs.rs/async-stream/0.3.6/async_stream/)
for that general mechanism rather than a custom producer/channel/state machine
or another hidden SQLx helper. The registry reports release 2024-10-01,
MSRV 1.65, MIT, no features; its companion proc macro is pinned to 0.3.6.
The upstream Tokio repository was unarchived and last pushed 2026-09-05.
The [resolved implementation](https://docs.rs/async-stream/0.3.6/src/async_stream/async_stream.rs.html)
owns the generator as a field and polls it from the stream; dropping the
stream drops that future and its checkout. The supported macro supplies
backpressure without a spawned producer or a channel backlog.
Existing futures-util supplies Stream/TryStreamExt/FutureExt, but unfold
alone cannot retain a borrowed SQLx row stream beside its owning connection
without additional ownership machinery. SQLx's internal TryAsyncStream would
add a second hidden dependency. No specific advisory was established in this
research; Implementation must run the repository's dependency gate for the
new lock resolution instead of treating this as a clean advisory scan.

For finite cleanup tasks reuse the already resolved `tokio-util` 0.7.19
TaskTracker (`rt` feature), plus existing Tokio and futures-util. Latest
registry release was 2026-07-21. TaskTracker tracks completion and discards
completed-task storage; it does not observe panic by itself. The adapter must
observe panic separately without recording its payload. JoinSet would require
an additional retained result set and draining loop. No new crate owns pool
policy, retries, or shutdown.

## Bounded local experiment

The [probe source](driver-probe.rs) and [raw log](driver-probe.log) are retained
as design evidence only. The source was temporarily copied to
`test/tests/__pool_driver_probe.rs` and removed by a finally handler after the
run. No production code, permanent test, manifest or lockfile changed.

Source SHA256:
`e0a4b388a68777162d26f245484f75c58d91d6ecbf84c76e9b201e3dfb513ae0`.
Log SHA256:
`be2b685bcfc599ae30b2c9a1a745c9b733d22052da84010e192daea858a37f6e`.
The command was run from this task worktree with the original PATH preserved
after prepending `/Users/daniil/.cargo/bin`:

```text
/opt/homebrew/bin/rtk proxy bash scripts/ci/validation-lock.sh -- bash scripts/ci/test-integration-db.sh --test __pool_driver_probe -- --nocapture
```

The PostgreSQL owner supplied the throwaway Compose lifecycle and per-test
database. The probe checked active SQL from an independent server connection,
then awaited relay acknowledgement that existing sockets had become silent.
New relay connections continued forwarding. No TCP reset was used to establish
the recovery claim. All probe-owned relay tasks were stopped and joined.

Observed server: PostgreSQL 18.6 (Debian 18.6-1.pgdg13+2), from the repository's
`postgres:18` image digest
`sha256:4ef4dbc939d61acea57712655ddb4b4ab27419c913f94cca0cd57cb3ea3c2280`.
The harness also started the configured PgBouncer 1.26.0 image digest
`sha256:d19bf5c8e785f602f5c0fb9ab570b58b47ac9699650f1976273a19411608bfd9`;
the probe used the direct PostgreSQL endpoint, so it makes no new pooler
runtime claim. The existing supported pooler proof remains Implementation's
regression input.

| Observation | Result |
| --- | --- |
| Native negative control: cancel active SQL, old socket silent, max one | Next acquire timed out after 3002 ms; size 1, idle 0 |
| Bound the owned return future to 1 s | Slot size reached zero; replacement acquired and SELECT 1 succeeded in 1035 ms from cancellation |
| Healthy bounded return | Next acquire used the same backend PID |
| Silence after a successful statement, before return ping | Deadline dropped the return future in 1002 ms; size zero |
| Drop unpolled owned return future on a silent connection | Size zero synchronously, measured 19 microseconds |

One test passed, zero failed; test body plus fixture cleanup took 10.66 s.
SQLx's fixture cleanup reported that its database still had users: local
socket/slot release is not immediate server-side cancellation of pg_sleep.
The outer Compose teardown then removed the throwaway environment. This
warning does not change the local capacity observation, and must not be
represented as server rollback proof. The probe's raw-return seam was driven
inline; the proposed tracked cleanup scheduling and adapter routing are not
implemented or proven by this run.

## Independent support and reopen

Fresh read-only specialist `/root/pool_design/driver_decision` used
`gpt-6-astra`, high, with no inherited history. It independently inspected the
driver source and supported the owned-return mechanism. Its bounded follow-up
confirmed that all ordinary guard drops can use bounded cleanup, including
cancelled SQL, readiness and COMMIT; only pending BEGIN needs immediate
discard. No per-statement completion flag is needed for R1.

Reopen the mechanism for any SQLx upgrade, different owned-future/drop order,
new nonzero-minimum requirement, failure of the permanent silence/reuse proof,
or permission for unmanaged transaction-control SQL on direct checkouts.
Retire the hidden seam when upstream provides a supported whole-return timeout
and acquisition hook meeting the same contract. These limitations return to
Technical Design, not a user implementation-choice question.
