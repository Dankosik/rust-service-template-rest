# Operational recovery baseline and disposition

Status: ready. Inspected 2026-10-06 at
`699887b18594088a59bcc23a049d290d089f6da1` in the task's isolated checkout.
This is supporting Definition research, with source inspection only; no build,
test or runtime experiment was executed by this actor.

## Decision and stop condition

Decide which accepted recommendations remain necessary after PRs #254 and #255,
and whether a concrete template-owned recovery gap remains. Stop when the
required-task, readiness, pool and consumer paths give Specification a falsifiable
delta without repeating an already accepted mechanism. Primary source below
is canonical; PR prose and historical artifacts are leads.

## Current evidence

| Fact and primary locator | Decision effect and limit |
| --- | --- |
| PR #254 merge `468353341db221741cffc05194053f49ec2c4779` is an ancestor of the baseline. Its runtime implementation is retained; #255 adds the current [Build Speed](../../../docs/build-speed.md) owner. | Integrate from current main. Do not cherry-pick the superseded logging/lifecycle/provider implementations. |
| The root independently read required check `112027991646` in [CI run 37387125913](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37387125913) and codeql-required `112027915792` in [run 37387125786](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37387125786), both SUCCESS for `9fec2c335d4f140bd4706bcc0379f882b4efcc2c`. | Reuse the observed gate results within their inputs. Their existence does not prove this follow-up or an uninspected artifact's numerical results. The [quota proof owner](../../../docs/runtime-progress-proof.md) remains canonical; no rerun is required merely because this specification exists. |
| [Service Background](../../../crates/service/src/bootstrap/shutdown.rs) retains `JoinSet`, a sticky first-failure latch, fallible manager registration and completion custody; [bootstrap](../../../crates/service/src/bootstrap/mod.rs) observes failure during startup/serving. [Worker Background](../../../crates/jobs-worker/src/shutdown.rs) retains tracker/abort custody, and [worker bootstrap](../../../crates/jobs-worker/src/bootstrap.rs) also observes engine/consumer failures. | Unexpected return/panic and explicit manager failure already initiate the process-owned cleanup path. Preserve this mechanism. A pending future does not drop its completion guard. |
| [Readiness](../../../crates/health/src/lib.rs) records completion on success, probe failure or timeout, bounds probes, uses Delay ticks, and rejects reader freshness beyond `probe_budget + 3 * max(interval, probe_budget)`. Its `refresh_until` loop can remain pending while its owner and previous state remain alive. Neither process root observes expiry as a primary failure. | Concrete gap: a core readiness driver that stops making completions can leave an indefinitely running unready process. Existing freshness withdrawal protects routing consumers but does not provide bounded process recovery, especially without continuous readiness ejection. |
| [gRPC health](../../../crates/infra-grpc/src/health.rs) Check and Watch consume the same reader; `wait_for_verdict_event` detects a stale edge without publication. The health reader's focused tests cover stale edges; [transport proof](../../../crates/infra-grpc/tests/transport.rs) covers initial/unready/drain Watch, and [process proof](../../../crates/service/tests/grpc_process.rs) covers TLS/HTTP/Watch shutdown and manager failure custody. | These are useful retained proof owners. They do not establish dependency-loss/recovery through a live Watch with successful useful work after recovery, nor lost-progress process recovery. |
| [PostgresProbe and pool](../../../crates/infra-postgres/src/pool.rs) share the ordinary pool; the probe can fail acquisition while the database remains responsive. [Persistence](../../../docs/architecture/persistence.md#readiness) deliberately keeps this policy and documents correlated withdrawal risk. [The saturation case](../../../test/tests/postgres.rs) holds one pool slot, observes three failed checks, releases it, then proves a transaction and readiness recovery at unchanged capacity. | Preserve pool-aware readiness and current defaults. Existing real-database coverage is one pool plus a direct reader/transaction, not two instances or HTTP/gRPC consumer recovery. Capacity/threshold retuning needs workload evidence, not an unrelated PR description. |
| [Jobs engine](../../../crates/infra-jobs/src/engine.rs) owns task failure reporting and the 12 s database-operation backstop; native consumer/manager owners retain their failure policies. [Runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md#integrating-process-owned-work) requires managers to report live failure while retaining completion custody. | Reuse existing operation deadlines and owner failure channels. Empty queues and idle listeners are not proof of a stuck task; do not invent one generic progress counter/deadline for all work. |
| [Lifecycle tests](../../../crates/service/tests/lifecycle.rs) cover application connection-cap refusal while diagnostics answers liveness. [Runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md#readiness-and-liveness) still gives unconditional diagnostics-port advice; both listeners use the same scheduler. | Keep separate connection-cap semantics, qualify topology guidance, and close the no-diagnostics recovery observation gap. Diagnostics isolation does not establish scheduler isolation. |
| Current [Build Speed](../../../docs/build-speed.md) owns scoped type-check-first iteration, one completion pass, worktree-local targets, no cache clearing without corruption, and supported compiler caching. Root's machine inspection found line-table debug configuration but no active sccache wrapper or sccache on PATH. | The policy is adopted, but this is not an implemented compiler-cache capability on the current machine. Add supported opt-in command-scoped caching through the existing build entry points; Technical Design owns safe provision/configuration without global settings or shared targets. Do not claim hits or a warm build from documentation. |
| [Validation lock](../../../scripts/ci/validation-lock.sh) uses one Git-common directory, records wrapper PID/candidate/argv and polls every second. It prints holder data only on its default 900 s timeout. `INT`/`TERM` cleanup removes the lock without explicit child-completion accounting; competing stale reclaimers may remove metadata after a successor acquires. | A concrete existing-script delta is warranted: visible bounded waiting, safe cancellation/release and stale-owner recovery while preserving mutual exclusion. These are source-path findings, not reproduced runtime failures. The next owner must resolve the mechanism; no daemon or replacement validation framework is implied. |
| [Verification runner](../../../scripts/ci/verify.sh) preflights binary/Docker availability, fingerprints source/plan/toolchain and records step results. It has no selected target/cache or disk-capacity preflight, and failure output only says failed command. [Validation routing](../../../docs/validation-routing.md) already retains partial attempts and reuses exact passing receipts. | Preserve that evidence owner; expose current build/cache/resource inputs and distinguish unavailable resources/ENOSPC from a code assertion failure. Newly supported cache/output settings must participate in the appropriate execution identity; no global cache deletion is a recovery action. |

## PR #243 hunk disposition

Compared its head `d200ef09ee88014995e2b07515a16340d049459b` with both its
`5927ffb` base and the current baseline, rather than comparing branch titles.

| Original concern | Current disposition |
| --- | --- |
| Freshness-aware failure absorption and completion/stale-bound metrics | `crates/health/src/lib.rs` is byte-identical between #243 head and current baseline. Already adopted, including focused coverage. |
| Bounded session readback and rejection cleanup | Adopted and superseded by `prepare_pool`/`admit_pool` ownership: one 5 s admission including acquire/readback, then bounded 5 s convenience-close. The root retains pools before awaiting. Do not restore #243's older 3 + 5 + 5 allocation or ownership. The silent-readback test is retained and extended. |
| Health ownership/configuration and responsive-pool recovery description | Adopted in current boundaries, configuration and persistence owners; retain current stronger lifecycle. |
| Probe timing explanation | Still needed: runtime guide says about 6/12 s; #243 accounts for phase plus serial rounds, yielding illustrative default bounds of about 6/11/14 s under a runnable scheduler. Preserve the separate 16 s stale bound and actual cadence authority. |
| Listener/platform guidance | Still needed: explicit shared scheduler, absence of readiness on diagnostics, no-diagnostics capacity caveat, and distinction between continuous readiness and deployment-only health checks. |
| Old shutdown-tail text | Superseded: current service/worker owners use the 18.5 s whole tail and current cleanup voting. Never restore #243's historical tail wholesale. |
| Historical specs/completion receipts | Provenance only, not current execution authority or current-head validation. Delivery Design chooses updating #243 versus a new PR with explicit supersession. |

The official [Kubernetes probe documentation](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-probes/)
distinguishes readiness withdrawal from liveness restart. Current
[Railway healthcheck documentation](https://docs.railway.com/deployments/healthchecks)
says healthchecks run at deployment start rather than continuously. Retrieved
2026-10-06; no live platform setting was inspected or changed.

## Counter-evidence, unknowns and refresh

Existing task termination supervision disproves a need for a second supervisor.
Completed failing readiness rounds disprove equating dependency outage with a
dead refresher. Existing saturation recovery disproves a blanket claim that the
pool needs replacement or larger defaults. Existing gRPC stale-edge support
disproves a new polling framework. The remaining failure transition and consumer
evidence are bounded deltas over these owners.

No production fleet, platform restart policy, real workload capacity, or fresh
runtime execution was inspected here. The root supplied current gate readback;
numerical quota receipt reuse still requires its actual source/configuration
identity and scope to match any later claim. Refresh affected rows when baseline,
retained profiles, current owners, provider contract or accepted behavior changes.
Design owns mechanism/placement and delivery disposition; Implementation owns
the proving cases and commands.
