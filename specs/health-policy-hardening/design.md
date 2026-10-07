# Health-policy hardening: Technical Design

Status: ready

Authority: [Intent](intent.md), [ready Specification](spec.md), and
[Definition result](definition-result.md). Specification SHA-256:
`0e36e32ef020b8f7aee5aa4012aced2ef8aec761834fbb862945a470d50b8c56`.
Baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, branch
`codex/health-policy-hardening-20261005` in its isolated worktree.
This artifact closes mechanisms and placement for R1–R4; it does not change
their behavior or claim execution proof.

## Decisions and material flows

### R1: the existing publication fold checks freshness

Keep `Readiness`, its watch-channel `State` / `Check`, and `ReadinessReader`
as the sole owner and projections in [health](../../crates/health/src/lib.rs).
`refresh` already captures Tokio `Instant::now()` after `check_probes` finishes.
Pass the existing policy's `stale_after` into the private
`apply_failure_threshold` fold. Retain Ready on a failed observation only when
the previous check is Ready, `at.duration_since(previous.at) <= stale_after`,
and the resulting failure streak is below the existing threshold. A stale
previous publication immediately publishes the new failure with its ordinary
reason and completion instant. Preserve streak progression and reset on success.
Do not reset the previous timestamp before making the freshness decision.

The flow is unchanged: caller-owned probe round → completed observation →
fold under the watch publication lock → `Check` publication → readers and
existing transition/late-refresh logs. A round beginning fresh can finish stale;
the completion instant decides. A previously absorbed failure is a publication
and expires by the same rule. No reader visit or extra stale bit is needed.
Cancellation before completion publishes no new check. Existing drain precedence
and the locked Ready-gauge write remain intact; a completed round during drain
may update the completion record but never re-enable traffic.

The fold's freshness condition replaces the current comment that staleness is
only reader policy. HTTP status/body, gRPC projection/Watch stale notifications,
probe registration, parallelism within a round, serial rounds, and Delay missed
ticks stay with their existing owners. No transport or bootstrap edit is needed.

### R2: emit completion time and the existing bound

The operational question is whether the last completed evaluation is still
fresh when the refresher stops. Add two unlabelled gauges through the existing
`metrics` facade in `health`, described alongside its existing metrics:

| Metric | Units and initialization | Writer |
| --- | --- | --- |
| `readiness_last_completed_timestamp_seconds` | Unix seconds as `f64`; `0` means no check has completed in this process | `Readiness::new` seeds zero; a completed `refresh` records wall-clock completion time |
| `readiness_stale_after_seconds` | Duration in seconds as `f64`, from `policy.stale_after()`; current default 16 | `Readiness::new` |

Sample `SystemTime` at the same completion point as the existing monotonic
`at`, then write the completion gauge inside the existing `send_modify`
publication closure, beside `readiness_ready`. Do not retain wall-clock state
in `Check`, add a new shared owner, or add a metrics loop/handler callback.
Every completed success, failure or timeout updates the timestamp. Starting a
round, cancellation, and `start_drain` do not. A check actually completing during
drain still updates it. Failure freshness says nothing about probe success.
Use seconds since `UNIX_EPOCH`; if the sampled time is not a positive usable
Unix timestamp, emit `NaN` rather than panic or reuse the zero/old value.
`NaN` means a completion occurred but the wall clock cannot date it.

With a current successful scrape and comparable clocks, let `T` be the last
completion timestamp, `B` the stale bound, and `N` the observer's Unix time.
`T == 0` means NotEvaluated; `T > 0` and `0 <= N - T <= B` means fresh;
`T > 0` and `N - T > B` means expired. The Prometheus form of the last rule is
`(time() - readiness_last_completed_timestamp_seconds > readiness_stale_after_seconds)
and (readiness_last_completed_timestamp_seconds > 0)` with the usual per-target
label matching. `readiness_ready` retains its existing meaning as the published
verdict, including a possible frozen 1. These new metrics are observations,
never inputs to endpoint readiness.

Document limits at the metric owner: application clock jumps or collector skew
can overstate/understate age; a future timestamp or NaN is unknown freshness,
not Ready. Missing samples or a failed scrape are unknown observation, not
NotEvaluated; use scrape health and sample recency. Scrape/evaluation cadence
adds delay. Individual gauge writes are not an atomic multi-metric snapshot:
a scrape crossing publication may mix adjacent completions. The health reader
uses its existing monotonic clock and precedence regardless of these limits.
The existing process-wide single health-owner convention and metric labels
remain; no new per-owner identifier/cardinality is introduced.

Timestamp plus bound is the smallest useful additive representation. An age
gauge updated only by `refresh` would freeze; a scrape-time age collector would
couple the health state into telemetry; a watchdog/second writer would add a
lifecycle owner. None meets the current requirement more simply. Accepted cost:
wall-clock interpretation and scrape limits, consistent with the
[Prometheus timestamp guidance](https://prometheus.io/docs/practices/instrumentation/#timestamps-not-time-since).
Reopen this representation only for an accepted requirement for clock-independent
metric/endpoint equivalence, not to hide its limits.

### R3: bound admission before returning the native pool

Keep all mechanisms in [infra-postgres pool.rs](../../crates/infra-postgres/src/pool.rs),
which already owns `connect`, `verify_session`, `ConnectError` and `close`.
Add private named constants `SESSION_VERIFICATION_TIMEOUT = 5s` and
`SESSION_REJECTION_CLOSE_TIMEOUT = 5s`. No config key or new exported helper is
needed. Add the public enum variant
`ConnectError::SessionVerificationTimeout { budget: Duration }`, rendered as
`postgres session verification: did not complete inside the {budget:?} budget`.
It contains no driver error/source, DSN, credentials, or server payload; its
type and stage distinguish it from initial `ConnectError::Timeout`.

After existing initial pool acquisition succeeds, wrap the entire
`verify_session(&pool, options)` future in one `tokio::time::timeout` call.
That future owns its acquire, complete settings query and existing validations;
both Startup and Server modes pass through this one boundary. Do not place
separate resetting timers around individual reads or acquire. Elapsed maps to
the new variant. A normal returned verification error retains its existing
identity and mismatch rules; the new timeout never formats a driver error.
Success alone permits `Ok(pool)`.

For every verification rejection, including timeout, retain the original
`ConnectError`, call the existing `close(&pool, SESSION_REJECTION_CLOSE_TIMEOUT)`,
then return that error for either `Closed::Complete` or `Closed::TimedOut`.
The latter may emit one bounded structured warning
`postgres_session_rejection_close_timed_out` with its budget only; it must not
emit `postgres_pool_closed` or replace the admission failure. No extra cleanup
retry, detached task, or unverified pool is introduced.

The connection borrowed by `verify_session` drops when the timeout drops its
future. Native SQLx return/close behavior remains the connection-resource owner;
the existing adapter close helper bounds waiting. Local handles then drop on
the error return. External cancellation drops the caller-owned connect future
and local resources as today, without spawning a finishing task or promising
that async cleanup was awaited. Only an observed successful close establishes
completed local pool cleanup; expiry does not establish physical remote socket
termination. Session readback is read-only, so no durable write/replay or unknown
business commit is introduced.

Allocated waits are sequential: initial acquisition up to 3s, verification up
to 5s, rejection cleanup up to 5s (13s total). This is neither a bootstrap
deadline nor a process-exit bound, and relies on a runnable yielding scheduler.
The server's 8s statement timeout does not bound a silent network. Read-only
migration-history admission remains a separate existing 5s startup step;
`connect_session`, shutdown budgets, transaction budgets, and the vendored SQLx
5s native connection-return bound are unchanged.

Existing APIs suffice: locked Tokio 1.53.1 supplies
[timeout cancellation](https://docs.rs/tokio/1.53.1/tokio/time/fn.timeout.html),
metrics 0.24.6 supplies gauges through the already-installed Prometheus exporter
0.18.3, and SQLx 0.9.0 supplies the native pool. The resolved vendored
[Pool::close documentation](../../vendor/sqlx-core/src/pool/mod.rs) describes
the optional wait and remaining-handle lifetime; the adapter already wraps it
in a Tokio timeout. A new health-manager crate, generic admission helper,
client wrapper or dependency upgrade supplies no missing mechanism. Reopen
only on evidence that a supported deployment cannot meet the accepted bound.

### R4: update guidance at its existing owners

Runtime guidance must explain completion-time freshness smoothing, the two new
metrics and their limits, unchanged published metric/log meanings, and the
stopped-versus-ended-task distinction. Correct the timing estimate: include the
initial phase up to 2s before the next check, serial rounds under Delay scheduling,
parallel probes within the common deadline, and the third failed result.
Current estimates are about 6s for fast failure, 11s for 3s pool-acquire failure,
and 14s for 4s round-budget exhaustion. They are policy estimates under a runnable
scheduler, separate from the 16s stale guard and additional platform polling.
No tuning decision follows from these numbers.

Retain Railway deployment-promotion-only health semantics and distinguish a
continuous readiness consumer such as Kubernetes. Explain that diagnostics
liveness/metrics has a separate connection cap but shares the runtime, provides
no readiness route, and that without it application connection saturation can
prevent both probes from answering. No platform setting is changed or claimed
observed. Persistence and configuration guidance describe the sequential
admission/cleanup budgets, mandatory verification, and unchanged history step.

## Responsibility and inverse file map

Placement is forced by the existing owners; no new crate, module, dependency,
directory or cross-crate edge is justified. This map is sufficient for one
bounded Implementation unit; Planning selects the unit and its execution owner.

| File / owner | Present reason and boundary |
| --- | --- |
| `crates/health/src/lib.rs` | R1 private fold and caller, R2 metric descriptions/emission, matching crate docs; existing colocated health/metric proof surface |
| `crates/infra-postgres/src/pool.rs` | R3 private constants, timeout variant, verification/cleanup sequencing, matching API docs; existing colocated admission/error proof surface |
| `test/tests/postgres.rs` | Existing real PostgreSQL admission and silent-relay proof surface for the changed pooled path |
| `test/tests/support/commit_proxy.rs`, only if required by selected proof | Extend the current relay narrowly if its present hooks cannot target session readback; no new harness, runner, or production test seam |
| `docs/architecture/runtime-lifecycle.md` | R1/R2 and R4 operating semantics, metric interpretation, detection estimates and listener/platform limits |
| `docs/architecture/persistence.md` | R3 canonical persistence budget table and pooled-admission contract |
| `docs/configuration-source-policy.md` | Link the new code-owned budgets and freshness rule to their existing authorities; no new key |
| `docs/architecture/boundaries.md` | Small consistency edit to the health owner's listed freshness metrics |

Bootstrap, config types/defaults, telemetry collector, HTTP/gRPC adapters,
generated contracts, migrations and vendor code need no change. Public change
is the additional typed error variant and additive metrics; `connect`'s
signature/native pool type remain. Keep template profile markers and generation
authority intact. If a concrete compiler/caller consequence requires another
file, update only this map and recheck that ownership delta; an actual mechanism
or behavior change returns to its owning phase.

## Proof and movement boundary

The existing health tests can observe the fold, monotonic age boundaries and
metrics without I/O; the existing PostgreSQL tests and fault relay can exercise
the public connect path after initial establishment while a readback reply is
withheld. Existing startup/server setting mismatch and pooler proof remain
relevant. The counterexample is bounded return with the original timeout while
the peer is still silent, not return caused by tearing down the fixture. Cleanup
expiry must retain the original rejection. Actual PostgreSQL observations use
[PostgreSQL Validation](../../docs/validation/postgres.md); fake time or a mock
alone cannot establish that claim. Local return is not remote socket finality.

Implementation chooses concrete cases, fixture refinements, assertions and
commands, reusing adequate coverage. Builds, tests and documentation checks run
at its existing final-validation boundary. No new per-task gate, fleet matrix,
watchdog qualification or extra environment is created by this design.
This phase performed source/API and static consistency review only, no tests or
builds. Required independent review is recorded in [Design result](design-result.md).

Reopen Specification for changed observable policy or an incompatible 5s bound;
reopen this design for a failed mechanism/ownership assumption; Intake for
changed requester meaning or external authority. Commit/push/one PR authority
continues downstream; merge, deployment and infrastructure remain excluded.
