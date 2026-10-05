# Supporting baseline for Tokio progress

Valid as of 2026-10-05; checkout base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. Source observations establish risk,
not measured production stalls. Refresh when these source owners or versions
change. This is supporting Definition evidence, not a standalone research gate.

| Claim | Primary authority and limit | Decision effect |
| --- | --- | --- |
| Upload wrapper may consume unlimited ready empty frames in one call | [`ExactLength::poll_frame`](../../../crates/infra-object-storage/src/body.rs), the `len == 0` continuation and final-frame confirmation loop | Bound wrapper work per poll without changing length or trailer semantics. The underlying body's own poll is outside wrapper control. |
| Enabled logs write stdout synchronously on the event thread | [`install_subscriber`](../../../crates/infra-telemetry/src/logging.rs) selects stdout JSON and default text writer; [`JsonLayer::on_event`](../../../crates/infra-telemetry/src/logging/json.rs) invokes `write_all` | Isolate sink I/O and define bounded admission/loss/lifetime. Formatting remains synchronous. |
| The logger has multiple binary consumers | CodeGraph callers of `install_subscriber`: service bootstrap, jobs-worker bootstrap, migrate main; panic hook also uses this subscriber | Every consumer must retain output custody through terminal records. |
| Current loss and shutdown contracts do not define a buffered stdout writer | [Logging policy](../../../docs/configuration-source-policy.md#logging), [runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md) | Close degradation and flush outcomes in Specification; mechanism belongs to Design. |
| Blocking execution can outlive the waiter and runtime shutdown wait | [Tokio 1.53.1 spawn_blocking](https://docs.rs/tokio/1.53.1/tokio/task/fn.spawn_blocking.html), resolved by Cargo.lock; fetched 2026-10-05 | Guidance must hold admission until execution ends and retain an owner; timeout is not cancellation. The shared pool is not a CPU concurrency policy. |
| Existing method text is underspecified | [`rust-tokio`](../../../.agents/skills/rust-tokio/SKILL.md), paragraph saying to bound input and let callers enforce deadline; [job cancellation](../../../docs/background-jobs.md#run-handlers-and-complete-a-database-effect) already warns work may continue | Clarify actual-execution lifetime without replacing existing durable effect semantics. |
| Nonblocking logger has historical throughput costs | [2026-09-29 experiment](../../../docs/infra-telemetry-performance.md#2026-09-29-json-layer-static-filter-histogram-upkeep): reported saturation improvement but one-connection regression for tracing-appender | Counter-evidence prevents claiming a speedup or automatically selecting this library. The new stalled-sink progress requirement still warrants Design re-evaluation. No new benchmark threshold is accepted. |

The prior audit supplied by the coordinator found no present CPU-heavy business
algorithm or CPU executor. It also established typed worker sizing and the
pinned Tokio/toolchain versions. Those findings justify keeping a new CPU pool
out of scope; they are not a guarantee about future features. The existing
runtime-lifecycle summary's unconditional `available_parallelism` description
is stale relative to the typed override and should be aligned with the current
[configuration authority](../../../docs/configuration-source-policy.md).

No production incident, isolation latency target, saturation capacity target or
worker-count change is inferred from this evidence. Technical Design owns
library comparison, resolved-version compatibility, bounded writer capacity,
signals and lifecycle placement; it must reopen behavior if a viable mechanism
cannot meet the accepted loss/shutdown contract.
