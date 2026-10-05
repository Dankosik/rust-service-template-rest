# Technical design source evidence

Inspected in the assigned worktree on 2026-10-05 at base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. These are source/API observations,
not a product verification receipt. CodeGraph status was current. No build or
provider environment was started during design.

| Authority | Decision-changing observation |
| --- | --- |
| [Service bootstrap](../../../crates/service/src/bootstrap/mod.rs), [shutdown](../../../crates/service/src/bootstrap/shutdown.rs) | Subscriber/metrics errors precede current common cleanup; startup borrows a JoinSet and dependencies; bound listeners stay local until all binds succeed. Only serving watches background completion. Background join ignores JoinError; tail constants total 17 s. |
| [Worker bootstrap](../../../crates/jobs-worker/src/bootstrap.rs), [shutdown](../../../crates/jobs-worker/src/shutdown.rs), [public entry](../../../crates/jobs-worker/src/lib.rs) | Worker prepare awaits pool/engine stages without a common cancel select; provider stays local until Prepared returns. Startup abort omits telemetry; ReportUnlessCancelled hides panic after token cancellation; tracker spawn handles are dropped. Engines already own their failure/attempt cleanup. |
| Worker `Signals::install`, `wait` and `pending` in that shutdown owner | A detached application task forwards native signals into a watch counter. Its handle is dropped and channel closure becomes pending/no pending signal. Direct native stream ownership removes that unsupervised task while keeping the existing Signals responsibility. |
| [HTTP server](../../../crates/infra-http/src/server.rs) | Accept JoinHandle is only awaited in drain and before its timeout starts. Drop stops acceptance without connections; panic exits drain before connection cleanup. Hyper graceful close and connection tracker are existing mechanisms. |
| Existing `Drained::TimedOut` and `ServerError::AcceptTask` in that server | TimedOut contains only a connection count; AcceptTask requires a known JoinError. A separate AcceptTimeout is needed to distinguish absent accept acknowledgement from the exempt diagnostics scrape timeout. |
| [PostgreSQL pool](../../../crates/infra-postgres/src/pool.rs) | Native acquire is 3 s; session statement policy is 8 s. Session verification and error `pool.close()` are currently outside a whole client deadline. Session query is read-only and can use its single acquired connection. |
| [Resolved SQLx pool options](../../../vendor/sqlx-core/src/pool/options.rs), [pool](../../../vendor/sqlx-core/src/pool/mod.rs), [patch owner](../../../vendor/sqlx-core/PATCHES.md) | Resolved SQLx is 0.9.0 with the existing local sqlx-core patch. `connect_lazy_with` creates a retained native pool without waiting for connection, while `acquire` keeps native timeout semantics. Cancelled acquisition may drop a connection; pool close is an asynchronous operation. Existing return-path patch remains authoritative. |
| [Messaging](../../../crates/infra-messaging/src/messaging.rs) | `Messaging::connect` owns native client locally across `admit_topology`; failure close uses native drain and Closed watch. Keeping that client across caller cancellation requires the selected concrete admission holder. Consumer admission already borrows Messaging. |
| [Telemetry](../../../crates/infra-telemetry/src/traces.rs) | Installation stores a global provider clone. Current shutdown runs SDK shutdown via `spawn_blocking` and waits budget plus private 500 ms slack; timeout, SDK error and join failure map Incomplete. |
| [Runtime architecture](../../../docs/architecture/runtime-lifecycle.md), [budget policy](../../../docs/configuration-source-policy.md#runtime-budget-policy) | Root stage order, values and runtime 1 s allowance are existing policy; current 17 s validation does not count SDK slack/runtime tail. Service propagation is within drain, not a separate extra allowance. |

Locked native API sources were read from the installed Cargo registry:

- Tokio 1.53.1 `src/task/join_set.rs`: spawn returns `AbortHandle`;
  `join_next` is cancel safe; `abort_all` requests cancellation and does not
  join; `shutdown` discards panic outcomes, so explicit joins are needed here.
- Tokio 1.53.1 `src/runtime/task/join.rs`: task join/abort are distinct;
  running `spawn_blocking` cannot be aborted by its handle.
- Tokio 1.53.1 `src/runtime/task/harness.rs`, independently checked during
  Technical Design Review: `poll_future` drops a panicking future inside its
  unwind guard, so the task completion guard observes that poll panic during
  destruction, including after cancellation.
- Tokio-util 0.7.19 `src/task/task_tracker.rs`: `spawn` returns a JoinHandle,
  `wait` waits for closed and empty; dropping tracker does not abort tasks.
- Futures-util 0.3.34 `src/future/future/catch_unwind.rs` and feature-gated
  `FutureExt::catch_unwind`: catches each poll through standard `catch_unwind`;
  `std` enables it. The locked crate is MIT OR Apache-2.0, repository
  `rust-lang/futures-rs`. This is already selected, so no latest-release or
  upgrade claim is made.

`/opt/homebrew/bin/rtk proxy /Users/daniil/.cargo/bin/cargo tree --locked -e
features -i futures-util` completed with exit 0 and resolved one futures-util
0.3.34 with `std` already enabled through current consumers. The service has it
only as a gRPC-marked dev dependency; worker has `std`/`async-await` inside its
jobs marker. The design adds/moves those direct edges so correctness does not
depend on a different profile's feature unification. No manifest or lock bytes
were changed in this phase.

The accepted reuse decision relies on these exact locked APIs. It does not
claim current security advisory clearance, a provider shutdown observation, or
that all detached/blocking work terminates. Matching validation and final
review remain Implementation/delivery responsibilities.
