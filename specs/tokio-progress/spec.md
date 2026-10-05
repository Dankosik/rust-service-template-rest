# Tokio progress hardening

Status: ready

Requester meaning: [Intent](intent.md). Baseline and limits:
[supporting evidence](research/baseline.md). This is a behavior contract;
Technical Design owns mechanisms, numeric internal capacities and placement.

## Outcome and scope

Remove the two evidenced template-owned ways that cooperative request tasks can
stop making progress: unbounded consumption of ready empty upload frames in one
poll, and synchronous stdout writes from enabled log events. State a usable
discipline for future blocking/CPU-heavy work. Tokio cannot preempt arbitrary
synchronous business code; this change makes no scheduler isolation or latency
SLA claim for such code or for host-wide CPU starvation.

## Upload progress

`PutBody::stream` must preserve its exact-length contract while bounding the
amount of ready-frame processing its wrapper performs in one poll. A source
that continuously supplies empty data frames must let the caller regain control
after a finite internal work budget, including while confirming the end of a
body whose declared length has already been reached. If the wrapper yields
without receiving a pending result from its source, it must arrange a wakeup;
valid finite bodies must still complete without waiting for unrelated I/O.

Yielding is neither EOF nor length mismatch. Exact bytes, underflow/overflow
rejection, source-error propagation, held final data, trailers, size hints and
end-of-stream meaning remain as in the existing body adapter. The wrapper does
not guarantee progress when the source's own single `poll_frame` blocks or
performs unbounded computation. No new object-storage public error or REST
response is introduced.

Nearest falsifier: repeated ready empty frames monopolize one poll or fail to
wake after a cooperative yield; alternatively, inserting empty frames changes
the success/error result or order of the same finite upload.

## Logging progress and loss policy

For both configured JSON and text logging, after subscriber installation an
enabled record must not wait for stdout to accept bytes or for pending log
capacity to become free. This covers access, startup, shutdown and panic-hook
records emitted through the subscriber. Formatting still executes at the event
site: arbitrary user-provided `Display`/`Debug` work is outside this guarantee.
Raw CLI output, usage help and the pre-subscriber fallback are unchanged.

Pending log records have a finite capacity. At capacity, discard the newly
submitted complete record without waiting; retain previously admitted records.
Apply the same rule to every severity, including panic records. There is no
synchronous fallback to stdout or stderr and no unlimited retry/buffer path.
The bound is on pending record count, not a new maximum record length or a
promise of a global heap-byte cap. Existing record content limits still apply.

With a functioning sink and available capacity, preserve JSON field semantics,
trace correlation, filtering, SDK target caps and text formatting. Admitted
records from one producer remain ordered and concurrent records cannot be
interleaved into corrupt lines. Do not invent a total chronological ordering
between concurrent producers. Delivery becomes explicitly best effort: enqueue
is not durable delivery, sink failure can lose records, and a hard kill can lose
buffered records. Logging failure must not turn an otherwise successful request
into failure or change application readiness.

Loss caused by capacity exhaustion or a stopped writer must have a cumulative,
bounded-cardinality count available without using the affected stdout path;
service/worker diagnostics expose it where metrics are enabled. Detectable sink
write/flush errors and writer termination must be available to the lifetime
owner, without recursively logging through the same failed writer. Numeric
capacities, exact signal names and reporting API belong to Technical Design.

## Logging lifetime and shutdown

Every shipped subscriber consumer retains an explicit owner for the output
work through its last intended record. This includes normal completion, startup
failure, a stop during startup, and ordinary error returns after installation.
Do not terminate output custody merely because an async caller stopped waiting.

On ordinary shutdown, stop admitting after the last intended record and try to
drain admitted records within the process's existing shutdown budget. A stalled
sink must neither block Tokio workers nor extend that budget through a join or
destructor. The owner must distinguish completed flush from incomplete/failed
flush; a successful flush means accepted bytes reached the OS writer, not a
collector or durable store. A writer stuck in an OS call may survive until
process exit, which is allowed after the bounded wait and must be documented.

An incomplete or failed final flush turns an otherwise graceful service or
ordinary jobs-worker shutdown into the existing degraded outcome (exit 3).
For the migration command it turns an otherwise successful exit into failure
(exit 1) without implying rollback of committed migrations. Existing primary
error/usage outcomes retain precedence. Earlier overload drops alone do not
change the exit status. A dead writer during service operation is observed as
logging degradation; it does not independently terminate the application.

Nearest falsifier: a deliberately stalled writer prevents unrelated async work
or shutdown from completing; a short-lived consumer exits before an admitted
final record reaches a functioning sink; or the owner reports complete flush
after a writer failure. Admission-time loss remains independently observable.

## Guidance for business work

Update the existing runtime/contributor method owners, rather than introducing
a new executor API. They must distinguish short bounded synchronous work,
blocking I/O, sustained CPU work and loops over immediately ready futures.
An `await` is not proof that a loop yields; increasing Tokio worker counts or
the blocking-thread limit is not a substitute for bounded work and admission.

For future blocking/CPU-heavy work, require bounded admission before submission,
with its capacity held until actual execution ends, including after request
timeout or waiter cancellation. Give the work an owner that observes completion
and panic, requests cooperative cancellation where supported, and accounts for
remaining execution at shutdown. Started blocking closures are not stopped by
aborting their handle or timing out the wait. Repeated job attempts must not
assume earlier blocking work has ceased; existing effect/fencing rules remain
authoritative. Admission wait/rejection, concurrency and cancellation policy
belong to the concrete feature's accepted workload and budget.

Teach that Tokio's shared blocking pool also serves library file/DNS work and
that sustained CPU demand may justify a separately bounded CPU executor only
when an actual workload provides evidence. Cooperative yields do not make
blocking calls nonblocking and do not reserve CPU for another task.

## Deliberately unchanged and non-goals

Typed `APP__RUNTIME__WORKER_THREADS`, its unset startup default, request budgets,
load shedding, runtime-shutdown policy and dependency pools are unchanged.
Correct stale descriptions of those owners encountered within this scope.
There is no new global CPU pool, Rayon dependency, runtime knob, mandatory remote
benchmark campaign, broad JSON/crypto offload, infrastructure change or promise
to contain arbitrary future business code. Existing database behavior and
public OpenAPI remain unchanged. The historical logging throughput tradeoff is
acknowledged, not promoted into a performance acceptance threshold.

## Composition and completion

A request may upload a valid stream containing many empty frames while access
logs target a stalled pipe. The upload wrapper returns control cooperatively;
logging retains only its bounded admitted backlog and drops new records with
loss accounting; unrelated tasks and stop handling still progress. Shutdown
ends within its existing budget and exposes an incomplete flush through the
existing failure/degraded outcome, never a false success claim.

Implementation chooses focused proof at the smallest useful layer for upload
progress and compatibility, logging saturation/stall/lifetime, unchanged record
semantics, and caller exit outcomes. Follow repository validation routing and
final assembled concurrency-safety review; no runtime or throughput claim can be
inferred merely from compilation. Technical Design must research maintained
writer mechanisms before selection and close startup/teardown ownership for
all subscriber consumers. Reopen Specification only if a proposed mechanism
requires different loss, completion, compatibility or budget behavior.
