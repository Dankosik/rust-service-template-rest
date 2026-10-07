# Health-policy hardening

Status: ready

Requester meaning: [Intent](intent.md). Factual baseline:
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`; reviewed Research R2 supplied by the
continuation coordinator, with the affected health fold and PostgreSQL connect
path re-read during Definition. This is a behavior contract, not runtime proof.

## Outcome and boundary

Repair false readiness after stale evaluation, expose evaluation freshness,
bound pooled PostgreSQL session admission, and correct the affected operating
guidance. No new dependency, runtime controller, operator key or platform
configuration is required. Existing Tokio, metrics and SQLx mechanisms and the
current health, adapter and bootstrap owners remain the starting point;
Technical Design owns their exact use and placement.

## R1. Staleness and failure smoothing compose

The authoritative reader remains the health owner's cached state. Precedence
stays `Draining > NotEvaluated > Stale > published verdict`. A check is stale
only when its age is **greater than** `stale_after`; equality remains fresh.
The bound stays `probe_budget + 3 * max(interval, probe_budget)` (16 seconds for
the current 2-second interval and 4-second budget).

On completion of a new check, evaluate the previous publication's age at that
completion instant. A failed or timed-out check may retain Ready under the
existing failure threshold only if that previous publication is both Ready
and fresh. A previous stale Ready cannot absorb even the first new failure.
Publish that failure immediately with the ordinary failure reason and new
completion time. Further failures remain unready; a successful round restores
Ready and resets the consecutive-failure streak. No reader request is needed
to notice expiry before applying this rule.

| Previous state at completion | Completed check | Published result |
| --- | --- | --- |
| Never evaluated | Failure or timeout | Unready immediately |
| Fresh Ready, resulting streak below threshold | Failure or timeout | Ready, as today |
| Fresh Ready, resulting streak reaches threshold | Failure or timeout | Unready |
| Stale Ready, including a previously absorbed failure | Failure or timeout | Unready immediately |
| Unready, fresh or stale | Failure or timeout | Unready |
| Any non-draining state | Success | Ready, failure streak zero |
| Draining | Any | Reader remains Draining; refresh cannot re-enable traffic |

This applies when a round starts fresh and finishes after expiry too. Failed
completion still refreshes evaluation freshness; freshness is not success.
Before that completion, readers continue reporting Stale. HTTP retains its
200/503 contract and gRPC retains its shared readiness projection, including
Watch's existing stale-event behavior; no adapter gains independent policy.

Nearest falsifier: Ready, wait past expiry, then fail below the threshold;
the observable verdict must stay unready until a success. Also preserve the
fresh-threshold case, exact age boundary and drain precedence without duplicating
adequate existing coverage.

## R2. Operators can distinguish a fresh publication from a frozen one

Add an operator-readable freshness signal for the most recent **completed**
readiness check and expose the applicable stale bound with explicit units.
Operators using a current metrics scrape must be able to determine whether no
check has completed, the last completion is fresh, or it has expired, even if
no further refresh completes and no health endpoint is polled. A failed check
counts as a completion; an in-progress or cancelled check does not. Initialization
must not look like successful evaluation. Drain must not forge a new completion.

Keep `readiness_ready`'s existing published-verdict semantics and existing metric
labels compatible. It may retain 1 when the refresher stalls; document that it
must be interpreted with freshness, not as the time-adjusted endpoint answer.
Existing completed-check counters, probe outcome counters, published-transition
logs and late-refresh logs retain their meanings. Stale time passage alone does
not require a new log event or background writer. The new signal comes from the
existing state/publication owner and must not depend on a second monitoring loop,
health-handler side effects or dependency I/O. Technical Design selects the
metric names and representation and documents any clock/collection limitations;
those metrics do not become readiness authority.

Nearest falsifier: after one completed check the refresher stops while metrics
remain available; the documented interpretation must show expiry even though
the published Ready gauge remains 1. A fresh failed check must show current
evaluation without implying Ready.

## R3. Pooled PostgreSQL session verification has a client bound

Every caller of pooled `infra-postgres::connect` retains mandatory effective
session-budget/isolation verification before receiving a usable pool. After
initial pool establishment, session verification has a fixed **5-second**
client-side ceiling covering its pool acquisition and complete settings
readback. This is independent of the server's 8-second statement timeout and
applies to both startup-provided and server-provided session settings.
No response, partial response and acquisition wait cannot restart that ceiling.
There is no retry or fallback to an unverified pool.

Expiry rejects admission with a typed, sanitized session-verification timeout
distinguishable from the initial connection-acquire timeout. Error rendering
identifies the failed stage and budget without credentials, DSN or raw server
payload. Existing setting/isolation mismatch decisions remain unchanged.
For any session-verification rejection, request pool cleanup and bound the wait
to **5 seconds**; cleanup expiry cannot replace the original admission failure
with success or make rejection wait forever. Do not claim remote socket closure
when only local cleanup waiting expired. Cancellation keeps existing caller
ownership and must not spawn a detached verification/retry task.

The 5-second verification ceiling is a task-local policy choice aligned with
the existing startup-history admission ceiling for a small mandatory metadata
read, and allows acquisition within its existing 3-second ceiling. The cleanup
ceiling follows the existing dependency-close policy. Neither is a measured
latency SLO. Initial connect, verification and rejection cleanup are sequential:
their worst allocated wait is 3 + 5 + 5 = **13 seconds** under a runnable
scheduler; this is not a whole-bootstrap or process-exit deadline. Migration
session creation, history admission, readiness budgets, transaction budgets and
the native SQLx return bound are unchanged.

Nearest falsifier: initial connection succeeds, then the readback peer stops
responding; admission must return the sanitized timeout within its verification
plus bounded cleanup allocation, never a usable pool. Prove actual database
behavior using the repository's existing database validation owner and harness;
a mock result alone does not establish PostgreSQL behavior.

## R4. Guidance explains the actual limits

Update the affected runtime, persistence, configuration-budget and health metric
documentation at their existing owners. Explain R1–R3 and keep one consistent
metric/budget description. Detection estimates include initial phase before the
next check, serial rounds, parallel probes within a common round deadline, and
Delay missed-tick behavior without catch-up bursts. With current defaults and
a runnable scheduler, repeated fast failure is approximately up to 6 seconds,
3-second pool-acquire failure up to 11 seconds, and 4-second probe-budget
exhaustion up to 14 seconds. These are application policy estimates, not fleet
SLOs or bounds under arbitrary runtime starvation. Platform polling and failure
thresholds add delay; the stale limit remains a separate 16-second guard.

Keep explicit that Railway healthchecks gate deployment promotion, not ongoing
runtime ejection; a platform with continuous readiness such as Kubernetes can
withdraw unready endpoints separately from liveness. No live platform setting
has been inspected or changed. Preserve the diagnostic-listener distinction:
both application health routes bypass in-flight shedding but share connection
capacity; diagnostics serves liveness and metrics with a separate cap on the
same runtime and does not serve readiness. With diagnostics disabled, connection
saturation can prevent both application probes from answering.

## Recommendation dispositions

| Research recommendation or concern | Disposition and reason |
| --- | --- |
| Stale Ready can resurrect after failed refresh | Necessary: R1 fixes a concrete false-success path |
| Published gauge obscures stopped refresh | Necessary: R2 adds freshness context without changing authority |
| Session readback can hang despite server timeout | Necessary: R3 bounds admission and rejection cleanup |
| About-12-second hang detection claim | Necessary: R4 includes initial phase and scheduling assumptions |
| More regressions around interacting health rules | Necessary only for changed behavior and material uncovered failure modes in R1–R3; executor chooses smallest adequate proof |
| Restart hung refresher or restart on dependency outage | Unchanged: existing ended/panicked-task supervision exits failure; stalled refresh becomes Stale. No accepted service SLO justifies a watchdog or outage-triggered restart |
| Change cadence, budget, failure threshold or add breaker/retry | Unchanged: no measured fleet requirement supports tuning or extra state |
| Make all providers globally readiness-critical | Rejected for this PR: preserve service-owned criticality and current registration |
| Add business degradation when the core database fails | Service-owned: no accepted degraded routes exist; do not report Ready for unavailable required work |
| PostgreSQL working-pool saturation and recovery | Existing evidence retained: the one-slot saturation test exercises unready, real work and recovery. It is not fleet stability proof; repair the pool only if this patch reveals a concrete causal defect |
| Authentication/provider outage policy | Unchanged: cache hits survive only their current TTL/token expiry; misses use the provider; JWT retains last usable keys without max-age; no global readiness coupling |
| Cache, S3, telemetry and outbound provider availability | Unchanged: their existing local failure/degradation owners remain authoritative |
| gRPC Watch stale/recovery or no-diagnostics process tests | Add only if the chosen patch changes that behavior or exposes a material uncovered failure mode; existing projection/routing facts do not create a new gate |
| Fleet recovery, real platform routing, release stability | Future service evidence: requires deployment topology, load and SLO. Not a template PR acceptance claim or reason to mutate infrastructure |
| New health-manager library | Not needed: reviewed research found no machinery reduction over current Tokio/metrics/SQLx/tonic owners; reconsider only if Design exposes a concrete missing capability |

## Proof boundary and reopen

Acceptance requires focused regression evidence for the changed rules and the
repository-selected build/test/documentation checks at Implementation's final
validation boundary. Existing adequate proof is reused; new tests are not a
separate phase or a reason to multiply broad runs. No CPU-heavy verification
was run during Definition. Existing test source is evidence of coverage, not
a claim that it passed on this candidate.

Canonical factual anchors: [health](../../crates/health/src/lib.rs),
[pooled admission](../../crates/infra-postgres/src/pool.rs),
[runtime lifecycle](../../docs/architecture/runtime-lifecycle.md),
[persistence](../../docs/architecture/persistence.md),
[database validation](../../docs/validation/postgres.md),
[PostgreSQL proof](../../test/tests/postgres.rs), and
[gRPC transport proof](../../crates/infra-grpc/tests/transport.rs).

Reopen Specification if Design discovers a conflict between these observable
rules, the 5-second metadata-read bound is incompatible with a currently
supported deployment contract, or actual changes would require a new readiness
policy, wire contract or criticality. Reopen Intake for changed requester scope
or external authority. Otherwise Technical Design closes mechanism and ownership
without inventing new product behavior.
