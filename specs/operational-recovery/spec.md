# Specification: operational recovery

Status: ready after fresh independent Specification Review (PASS).
Authority: [Intent](intent.md); evidence: [current baseline and PR dispositions](research/baseline.md).
Baseline: `699887b18594088a59bcc23a049d290d089f6da1`.

## Outcome and recommendation coverage

The service must either keep making useful progress, recover an ordinary bounded
dependency/capacity failure in place, or enter the existing bounded failure exit
when required core progress is lost. Transport health and operator guidance must
tell these cases apart. A reachable listener or successful probe alone does not
establish restored useful work.

| Accepted recommendation | Required disposition |
| --- | --- |
| One coherent logging/lifecycle/reliability result | Preserve #254's canonical ownership and cleanup semantics; carry only #243's remaining timing/topology guidance and this follow-up's accepted delta. Delivery must state which PR supersedes or completes #243. |
| Required task recovery | Preserve current ended/panic/report policy and add the bounded core-readiness progress-loss behavior below to service and retained jobs-worker roots. |
| Overload, dependency outage and multiple instances | Preserve existing admission/pool defaults; distinguish observations and establish recovery/isolation at the declared two-instance boundary below. Numeric fleet sizing remains service-owned. |
| Real HTTP/gRPC consumers and no diagnostics | Close the consumer-observation boundary below, using the existing transport and lifecycle owners. |
| Verification/build/cache/resource improvements | Retain #255's iteration policy and deliver the existing-entry-point improvements in R4: safe observable lock waiting, opt-in command-scoped compiler caching and resource diagnostics. Documentation alone does not fulfil this recommendation. |

## R1. Required progress and process recovery

Unexpected return, panic or explicit failure of a registered process-lifetime
task continues to use its current sticky failure owner. A primary live/startup
failure remains exit `1`; requested-stop degraded cleanup remains `3`; clean
requested stop remains `0`. No task is silently restarted in place, and reporting
failure does not discard custody of admitted or blocking work.

Add a mandatory core-progress contract for the readiness driver in both runtime
composition roots, independent of diagnostics and retained dependency profiles:

- Arm after the first successful startup readiness completion, before a ready
  process may serve. Startup refusal before that completion keeps its current
  admission path; there is no new startup timer or configuration key.
- A completion means a finished round, including an ordinary probe failure or
  timeout. Starting a probe, a timer wake, a metric scrape or a cached read is
  not a completion. Use the same monotonic completion authority and bound as
  readiness freshness: `B = probe_budget + 3 * max(interval, probe_budget)`
  (16 s at current defaults). Equality is still fresh.
- While armed and before drain/stop, a gap strictly greater than `B` without a
  completion is a primary required-core-progress failure. It must be observed
  even when nobody polls HTTP, gRPC or metrics. A late completion must not erase
  the expired interval merely because it races the failure observer. Once
  expired, this process cannot publish recovery back to service readiness.
- The failure withdraws readiness and enters the existing single-deadline staged
  teardown, retaining its first primary cause. It emits a bounded, sanitized
  indication identifying readiness progress loss, and exits `1` within the
  existing grace allocation from observation under a runnable scheduler.
  Repetition cannot extend the deadline or overwrite the primary cause.
- Requested stop/drain disarms future progress obligations before cancellation
  suppresses completions. A gap already expired before stop remains primary;
  elapsed shutdown time alone must not manufacture a live-progress failure.
  Completion during drain cannot reopen readiness.

This closes the stuck-but-alive refresher case. The existing generic health
reader remains a reusable recoverable projection; the terminal policy belongs
to a supervised process. Ended/panic coverage and native provider/engine
operation budgets remain their present owners. Derived feature managers use
their actual operation/deadline semantics and the existing failure-reporting
capability; idle queues/listeners are not failures. This change does not pretend
to infer business progress from arbitrary callbacks or restart ancillary work
from a missing metric sample.

All in-process timers require scheduling. Complete runtime starvation can delay
both detection and cleanup; an external process supervisor remains the only
owner capable of enforcing a wall-clock restart during that condition. On
resumption, an already expired armed completion gap remains a failure. No
new thread, scheduler-isolated watchdog or platform mutation is required here.

## R2. Recoverable pressure and dependency failures

An HTTP/gRPC capacity refusal, an application-listener connection refusal, or a
completed failed/timed-out dependency round must not itself trigger R1 or restart
the process. Preserve bounded request admission, the current wire failure codes,
pool maximum, acquire/probe deadlines, failure threshold and recovery on one
successful fresh round. No new retries, unbounded waiting queue, health-only
database connection, admission knob or threshold retuning is accepted.

A shared-pool acquire failure establishes local inability to acquire, not a
database outage. A dependency connection/query failure also does not prove a
fleet-wide outage. Operator evidence must preserve the bounded failure class,
readiness completion/freshness and useful-work result, so these situations are
not flattened into an unexplained `503` or an allegedly healthy metric.

After responsive pool pressure is removed, the same process and pool capacity
must resume a real successful database operation and a fresh ready verdict.
After an injected dependency interruption ends, recovery likewise requires a
new successful dependency-backed operation; an old successful publication is
insufficient. Existing uncertainty rules for interrupted durable effects are
unchanged, and recovery must not replay an uncertain effect as known success.

At a bounded two-instance local boundary sharing the same dependency, pressure
confined to one instance must not withdraw the other instance's readiness or
prevent its useful work merely through template-local state. Correlated pressure
or dependency failure may withdraw both under current policy; completed failing
rounds must keep them alive for in-place recovery. Retain observed loss/recovery
and useful-work results for both, and do not claim fleet stability, optimal pool
sizes or production latency from that finite boundary.

## R3. Consumer and topology truth

HTTP readiness and gRPC Check/Watch continue to derive from the same canonical
readiness reader. A live Watch must communicate ordinary loss and recovery as
`SERVING -> NOT_SERVING -> SERVING` without a reconnect solely to see recovery;
loss of required core progress instead becomes terminal process withdrawal.
Drain still publishes `NOT_SERVING`, terminates Watch without holding drain, and
wins over a late success. No health consumer runs dependency probes per request.

Consumer proof must observe a useful request before failure and another newly
completed useful request after ordinary recovery, with the relevant dependency
actually on that path. Cover HTTP and gRPC at their real transport boundaries,
including the standard health Watch. The template need not gain a shipped
business endpoint for this proof; use its existing composition and fixture
owners. Keep database recovery, transport propagation and process ownership
claims separate where their proving boundaries differ.

With diagnostics enabled, it has separate connection capacity, serves liveness
and metrics, and has no readiness route; it shares runtime scheduling. With
diagnostics disabled, application liveness/readiness share application connection
capacity. Saturation can prevent either probe from connecting; after release,
the same process must resume admission and useful work. Required-core failure
and recovery policy must not depend on a diagnostics scrape/listener.

Guidance must condition probe routing on the actual platform: continuous
readiness ejection differs from deployment-promotion-only checking. State the
shared-scheduler and no-diagnostics limitations. Retain actual current budgets:
illustrative default failed-round detection includes initial phase and serial
rounds (about 6/11/14 s for fast/3 s acquire/4 s timeout), separate from 16 s
freshness and platform delay. Preserve the current 18.5 s teardown tail. None
of those estimates is a production SLO or permission to edit platform settings.

## R4. Delivery and evidence economy

Reuse current code and adequate proof with matching source/configuration scope.
Extend the smallest existing proving boundary for the actual missing behavior;
test cases, fixtures, assertions and commands belong to Implementation. A mere
counter, direct reader result or unrelated green CI cannot substitute for the
specific consumer/recovery claim. Builds, local runtime observations, inherited
gate results and this candidate's required CI remain separately identified.

Use the current Build Speed and Validation owners: each worktree owns its target,
type-check before completion builds, reuse built candidates and caches, and avoid
multiplying full builds across independent scenarios. Add only the following
functional improvements through the existing verification/build entry points:

- Preserve mutual exclusion across worktrees, including competing stale-owner
  recovery and owner changes. Waiting must promptly identify the observed owner,
  checkout and candidate, show elapsed waiting with bounded repeat output, and
  preserve the configured wait deadline. Omit sensitive command arguments and
  environment values. Timeout/cancellation runs no queued command and cannot
  release another invocation's lock. An interrupted active owner cannot declare
  the lock free while its governed validation work is still running; uncertain
  ownership must be diagnosed without killing unrelated work.
- Supply supported opt-in, command-scoped compiler-cache use for repeated builds
  while preserving per-worktree build outputs and normal Cargo correctness.
  Explicitly requested but unavailable or invalid cache configuration fails
  before the expensive command with an actionable cause; ordinary uncached mode
  stays supported. Record the actual wrapper identity and cache mode. Repeated
  compatible work may reuse native compiler artifacts; no first-build speedup,
  universal cacheability or cache hit may be claimed without an observation.
  Technical Design owns task-local provisioning and supported configuration;
  global Cargo/security changes are excluded.
- Before expensive work, expose the selected output/cache filesystem capacity
  and relevant build/cache locations, distinguishing unavailable observation
  from measured free space. Insufficient or exhausted storage must be reported
  as a resource failure with the pending step and retained partial evidence,
  rather than a code assertion failure or a pass. Diagnostic preflight alone
  does not guarantee enough space for arbitrary builds; a proactive low-space
  refusal requires an explicit supported caller resource requirement, not a
  guessed universal reserve. Preserve source, build outputs and shared caches;
  recovery does not automatically clean or prune.
- Preserve current candidate/plan/environment receipt reuse. Newly supported
  wrapper and output/cache configuration must be represented in execution
  evidence and its relevant identity so a different execution mode cannot
  silently borrow an incompatible passing receipt.

All new diagnostics and receipts use bounded safe labels, non-secret locators
or fingerprints. Never expose raw argv, environment values, DSNs or
secret-bearing wrapper/cache configuration. Existing caller-selected supported
compiler wrappers must not be silently replaced.

FIFO scheduling is deliberately unchanged: present evidence establishes opaque
waiting and unsafe ownership edges, not starvation requiring queue machinery.
Reopen ordering only on demonstrated starvation. A custom queue daemon, global target,
global cache installer or second validation runner is not needed. Tool absence
is a real capability state, not evidence that compiler caching is already done.

## Compatibility, exclusions and reopening

The intentional behavior change is fail-stop recovery of an armed readiness
completion gap beyond its existing freshness bound, including a late-resuming
driver. Ordinary dependency/capacity failures stay recoverable. The gRPC/HTTP
wire contracts, profile defaults, schema, job effect/fencing rules, logging
privacy and failure/cleanup ownership are unchanged except as stated above.

Technical Design owns the smallest implementation, placement, current library
reuse comparison if a new capability is proposed, and PR #243 delivery strategy.
No broad runtime replacement, new health framework, unrelated open-PR adoption,
platform rollout, database migration, global cache/security setup or numerical
fleet retuning is in scope. Reopen Definition if concrete evidence invalidates
the completion authority/bound, introduces an observable policy fork not settled
here, or changes requester scope. Reopen supporting Research for changed source
or evidence; missing implementation test choices do not reopen Definition.
