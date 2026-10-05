# Process lifecycle repair

Status: ready

Requester meaning: [Intent](intent.md). Baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
This is a behavior contract; Technical Design owns mechanisms and placement.

## Outcome and scope

The service and jobs worker retain one process lifecycle. Work registered with
that lifecycle receives cancellation and bounded completion handling, and a
partially started process releases the resources it has acquired. An operator
must not receive a clean-stop claim merely because the process stopped waiting.

The rules below repair demonstrated gaps, rather than making every research
recommendation mandatory. They apply to the ordinary long-running service and
worker entry points and shared adapters they use. One-shot operator commands,
schema, job settlement/replay, message delivery, REST/gRPC request semantics,
and profile selection remain unchanged.

## Required behavior

### L1. Stop during startup

Once stop handlers are installed, a stop observed during asynchronous admission
ends further admission and proceeds to cleanup without waiting for an unrelated
provider operation to complete. This includes pool/session/history checks,
engine admission, broker admission, and readiness evaluation. No new listener,
engine, or consumer starts after the stop is observed; resources whose startup
completed concurrently remain owned and are cleaned up. The stop deadline is
retained across the startup-to-cleanup transition, not restarted after a slow
admission. Synchronous local registration is required to return promptly; CPU
starvation or blocking a runtime thread is not made preemptible by this work.

Without a signal, PostgreSQL session admission and its failure cleanup have a
finite client-side bound, including a server that stops replying after acquire.
Session requirements are unchanged and failure refuses admission. Design must
derive the bound from existing admission/budget owners and record the arithmetic;
it must not add a configuration knob or rely solely on server-side SQL timeouts.

### L2. Partial startup keeps every acquired resource in cleanup ownership

A failure, stop, or caught unwind after acquiring a listener, background task,
dependency, or tracer provider must attempt that resource's applicable existing
cleanup stage. A later listener bind failure must therefore close already-bound
listeners and their live connections through bounded cleanup. Startup that never
became ready skips readiness propagation. It does not advertise successful
readiness or begin worker claiming/consuming after admission failed.

Every installed tracer provider receives an explicit bounded shutdown attempt,
including failures installing the subscriber/recorder and worker registration
or admission. Dropping a local handle while the global provider still exists is
not evidence of a flush. Cleanup errors do not replace the original failure.

### L3. Observe required task and listener failure

A process-owned long-running task that returns before its cancellation, or
panics, is a process failure; a configured listener's accept loop ending before
it is asked to stop is likewise a process failure. The running process observes
these failures without waiting for a later operator signal, withdraws readiness,
and follows staged teardown. A task failure already observed during startup
prevents a successful ready transition. Existing engine/consumer failure
reporting remains authoritative for those owners.

Normal completion after cancellation is expected. A panic during cancellation
or join remains an abnormal cleanup result and must be observable; cancellation
must not silently suppress it. Diagnostics name the owning task/listener or
stage using non-secret context; worker panic payloads remain withheld.

### L4. Completion, forced cancellation, and uncertainty differ

Cooperative background work is joined before its dependencies close. When the
join allocation expires, the owner requests cancellation/abort of controllable
async work and accounts for completion within the remaining existing budget.
If completion cannot be established, cleanup proceeds with an explicit
incomplete outcome rather than claiming the task has stopped. This applies to
both service-owned and worker-registered work; no second supervisor is needed.

The entire listener drain, including accept-loop completion, consumes its
supplied budget. Forced drain and timeout paths stop further accepting and
request connection cleanup. A timeout, dropped waiter, abort request, or runtime
shutdown return alone must not be reported as confirmed completion of all work.
Native library APIs continue to own Hyper, DNS, SQLx, NATS, Redis and SDK tasks.
Already-running blocking code may outlive a wait; the documented guarantee is
bounded process teardown handling, not that all such work was terminated.

### L5. Budget includes the whole process tail

One stop deadline covers ordered teardown, telemetry's existing 500 ms SDK join
slack, and the existing 1 s runtime shutdown allowance. Stage limits draw from
remaining time; a later stage cannot renew time consumed by an earlier one.
Configuration validation accounts for every sequential allowance: retaining
the present 17 s stages and slack gives a minimum of `drain_timeout + 18.5 s`.
The default 25 s drain therefore requires 43.5 s and still fits the unchanged
45 s grace. Configurations below the corrected minimum refuse before building
the runtime; the equality boundary is accepted. No duration default or
configuration file value changes. Bounds do not claim hard real-time scheduling.

### L6. Unwind does not skip teardown

A panic that unwinds through registration/bootstrap after lifecycle resources
exist follows their bounded cleanup and the explicit runtime shutdown path,
with process-failure disposition. This does not introduce recovery and resume
after panic: no further normal startup or serving follows it. Abort-mode panic,
double panic, process kill, and unrecoverable runtime starvation are outside the
guarantee. Existing payload redaction remains intact.

### L7. Terminal disposition is truthful

| Trigger/result | Exit disposition |
| --- | --- |
| Configuration, admission, startup, unexpected live task/listener failure, or caught bootstrap unwind | `1`, even if its cleanup also degrades |
| Stop signal with all outcome-voting cleanup completed normally | `0` |
| Stop signal with forced drain, background panic/failed join, or incomplete outcome-voting cleanup | `3` |

The current diagnostics-scrape drain-timeout exception remains: its bounded
forced close is recorded but does not alone vote degraded. An unexpected
diagnostics accept-loop failure before shutdown is still a live process failure
under L3. CLI usage/help mappings remain unchanged. Repeated signals keep their
existing expedite behavior and never restart cleanup or extend the deadline.
Stage logs distinguish completed, forced, failed, and unconfirmed work; a
success label cannot hide a join failure or missing provider flush.

### L8. Component integration is usable and scoped

Document where a service author supplies process-owned long-running work, how
it receives cancellation, which completion is expected, how early failure is
reported, and who releases dependencies. Keep the existing worker registration
contract. Service composition must offer an equally clear supported integration
path using its existing lifecycle owner. Whether that needs a narrow interface
or only reuse/documentation of existing wiring is a Design decision supported
by current consumers. An unused generic registration framework is excluded.

## Composition check and evidence boundary

A worker stopped while session admission is stalled must stop admission, avoid
claiming, clean acquired resources and flush telemetry under the same stop
deadline. If a registered task panics during that cleanup, the final result is
`3`; if admission had already failed, the retained primary failure makes it `1`.
A service whose second bind fails cleans its first listener even with a live
connection, flushes installed telemetry, uses explicit runtime shutdown, and
exits `1`. These scenarios couple L1-L7 and cannot pass on log wording alone.

Nearest feasible falsifiers are controlled pending admission/cleanup futures,
partial bind with a connected peer, task return/panic before and after cancel,
accept-loop completion, retained-provider shutdown observation, and boundary
budget arithmetic. Reuse existing unit/process fixtures; Implementation chooses
cases and commands under the repository validation budget. No new environment
or test harness is required by this specification. Observed database claims
remain governed by the persistence validation owner. Prior research runs are
baseline evidence only; Linux-only gRPC process tests ran zero cases on macOS,
and SQL/NATS/OTLP stall behavior was source analysis, not live-provider proof.

Source anchors reopened during Definition: [service bootstrap](../../crates/service/src/bootstrap/mod.rs),
[service shutdown](../../crates/service/src/bootstrap/shutdown.rs),
[worker bootstrap](../../crates/jobs-worker/src/bootstrap.rs),
[worker shutdown](../../crates/jobs-worker/src/shutdown.rs),
[worker registration](../../crates/jobs-worker/src/lib.rs),
[HTTP server](../../crates/infra-http/src/server.rs),
[pool admission](../../crates/infra-postgres/src/pool.rs), and
[tracer shutdown](../../crates/infra-telemetry/src/traces.rs).
The [runtime lifecycle](../../docs/architecture/runtime-lifecycle.md) and
[configuration policy](../../docs/configuration-source-policy.md#runtime-budget-policy)
must be reconciled with the repaired behavior during implementation.

No user-owned decision remains open. Reopen Specification if Design evidence
requires changing the observable contract; reopen Intake only if requester
meaning or authority changes.
