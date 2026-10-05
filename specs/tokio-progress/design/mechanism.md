# Tokio progress mechanism

Status: ready

Authority: [Specification](../spec.md), [Definition transition](../definition-transition.md).
Decision evidence: [writer comparison](libraries.md). Placement: [Ownership](ownership.md).
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`; branch
`codex/tokio-progress-20261005`. This design changes no production bytes.

## Upload polling

`ExactLength::poll_frame` receives an internal budget of 64 inner-body polls per
call. Check already-ended/held/trailers state before spending it. Before a 65th
inner poll, call `wake_by_ref` and return `Pending`; the next call starts with
64 again. Count every inner poll, including empty data and the poll after the
last held data frame. Preserve all fields across yielding; only existing EOF,
trailers, error and length checks may change their meaning. Source `Pending`
returns directly, relying on its waker. This works outside a Tokio runtime and
requires neither a spawned task nor a dependency change.

64 is a small internal work quantum, not a latency SLA or measured optimum.
It bounds wrapper work while allowing ordinary bodies to use the existing
fast path. Reopen the quantum if measured overhead or a real fairness target
requires it; do not tune it to this machine. An individual source poll and
arbitrary frame allocation remain outside the wrapper's control.

## Complete records and bounded admission

Keep the existing JSON layer and `fmt::layer().with_target(false)` text layer,
filters, trace bridge, ANSI policy and panic hook policy. Both resolved layers
already format one complete event before one `Write::write_all`; connect their
supported `MakeWriter` seam to the same private output writer. The queue message
is one owned `Vec<u8>` containing that complete formatted record. The writer
copies/enqueues once per `write_all`; it never breaks an event into chunks.
Its `write` has equivalent one-record semantics for this internal seam. Do not
export it as an arbitrary streaming writer or add a second formatting buffer.
Formatting failure retains the existing layer behavior; sink rejection returns
success to the layer after accounting loss so text's internal-error fallback
cannot synchronously print sink errors to stderr.

Use `std::sync::mpsc::sync_channel(1024)` and exactly one named standard thread
`service-log-output`. 1024 is the pending-record bound, plus at most one record
currently writing; producer formatting/copying is transient per active emitter.
It is not a byte bound. This intentionally small burst allowance costs less
retained backlog than tracing-appender's 128,000 default; no throughput or
burst-duration promise depends on it. No user configuration key is added.

The sole sender lives in a shared `Mutex<Option<SyncSender<Vec<u8>>>>` admission
gate. Formatting and byte allocation happen before acquiring this gate. Under
the gate, a producer only checks presence and calls `try_send`; there is no I/O,
formatting, await, capacity wait, or user callback. No sender clone escapes.
Drop the returned rejected bytes outside the gate. The gate gives admission
and close one linearization point; it is brief mutex contention, not a wait
for stdout or queue capacity and not a wait-free claim. A poisoned gate is
closed, never retried or panicked through, with stopped-writer loss accounting.

A full queue drops that newly submitted whole record and increments `full`.
A closed/disconnected writer drops it and increments `stopped`. Already admitted
records are never displaced. A single FIFO receiver serializes records and
preserves each producer's order; it creates no chronological-order claim for
concurrent producers. All severities follow the same policy. A single sink
failure may leave a partial line; fail-stop prevents subsequent records from
being appended after that incomplete line. Queue overload never changes request
success, readiness, or the process exit outcome by itself.

## Writer completion and custody

`install_subscriber` returns a `#[must_use] LoggingGuard` on success, rather than
`()`. Thread creation is fallible through a new `LoggingError` variant; invalid
filter, spawn failure and duplicate installation retain startup-failure mapping.
On duplicate installation close the unused writer immediately without waiting.
The private thread receives complete records and writes them with `write_all`
to stdout, then flushes before marking that record confirmed. It also flushes
on terminal drain. There is no additional `BufWriter`, background retry or
batching buffer. Per-record flush bounds failure accounting to the current
record plus the finite queue; stdout already line-buffers complete log lines,
but no throughput claim is made for the extra flush call.

The thread owns the receiver, sink and shared status until actual execution
ends. It marks a dequeued record unconfirmed until both write and flush return
successfully. A write or flush error sets a sticky terminal failure (bounded
error kind, no payload/message) and increments its operation counter. Before
discarding anything, the failure path takes the same admission gate and removes
the sole sender. After releasing that gate, it counts and discards the finite
remaining queue, plus its one unconfirmed record if present, adding each once
to `stopped`. No sender can race another admission after that close. Earlier
confirmed records are not counted again; a partially written current record
is conservatively counted as unconfirmed, not claimed wholly absent at the sink.
A scope guard owns this same close/count operation for panic or unexpected
termination, records failure without tracing/printing, and closes the one-result
completion channel. It must not silently drop an uncounted receiver. Sink error handling and
the worker's own failure reporting never call the subscriber. The process panic
hook can still enqueue its best-effort panic event; the worker holds no admission
gate while calling the sink, so that hook cannot self-deadlock on the gate.

`LoggingGuard::shutdown(deadline: std::time::Instant)` closes admission by taking
the sole sender under the same gate, then receives the terminal result with
`recv_timeout(deadline.saturating_duration_since(now))`. Closing does not enqueue
a token and cannot need queue capacity. Disconnection makes the worker drain
all already queued records and perform its final sink flush. Publish `Flushed`
only after successful writes, final flush, and sink teardown; publish `Failed`
for detected write/flush/panic/termination failures; expiration is `TimedOut`.
A completed failure is sticky even if the queue later becomes empty. Check for
an already available terminal result before returning timeout at zero budget.
A completion receipt proves OS-writer acceptance, never collector/durable storage.

The public shutdown method is explicitly for the outer synchronous entry thread,
outside `Runtime::block_on`, and must not be called on a Tokio worker. It performs
no unconditional `JoinHandle::join`, including after a completion notification:
thread-local teardown could still run. Keep the handle with the lifetime owner;
a nonblocking Drop closes admission and detaches it. Timeout has the same drop
behavior, so the sink/thread retain the receiver and state until they actually
finish or process exit. The guard does not pretend to cancel an OS write and
its destructor adds no second wait. Tests can release a deliberately stalled
sink and then observe actual completion; a production-stalled thread may remain
until process exit. This is one logging adapter, not an executor or CPU pool.

## Loss and failure evidence

`LoggingGuard::status()` returns a cheap cloneable `LoggingStatus` reader with
saturating cumulative atomics for dropped/unconfirmed records (`full`, `stopped`), sink
errors (`write`, `flush`) and a bounded writer state. It remains readable after
writer termination and is independent of the metrics recorder. `snapshot()`
does no sink I/O, allocation of unbounded messages, tracing, or readiness work.

`Metrics::with_logging(status)` attaches that reader to the existing metrics
handle. At each `Metrics::render`, publish the snapshot through the metrics
facade using absolute cumulative counter values and a running gauge:

| Signal | Labels | Meaning |
| --- | --- | --- |
| `service_log_records_dropped_total` | `reason=full|stopped` | Full counts rejected new records; stopped counts rejected submissions plus the failed writer's discarded backlog and unconfirmed current record, including losses before recorder installation |
| `service_log_sink_errors_total` | `operation=write|flush` | Detected failed sink operations |
| `service_log_writer_running` | none | 1 while the writer executes normally, 0 after terminal completion/failure |

Describe signals beside their owner; use no dynamic error labels. Both service
and worker attach the reader before exposing diagnostics. No monitoring task or
readiness dependency is added. Metrics-disabled consumers still own `status()`
and the final `LoggingShutdown` result. Metrics are current at scrape time,
not a promise that a scrape occurs after diagnostics close. The exit code is
the process-level final-flush signal; do not attempt to log a failure through
the failed writer or write a fallback to stdout/stderr.

## Terminal paths and existing budgets

The outer synchronous entry owner retains an optional `LoggingGuard` in a slot
borrowed by startup. Store the guard immediately after successful installation,
before metrics installation or any fallible/awaiting next step. Store the
absolute log-drain deadline separately. This prevents `?`, dropped startup
futures and intermediate result returns from destroying custody.

For service and ordinary worker shutdown, at the start of the existing telemetry
stage capture `D = min(grace deadline, now + TELEMETRY_FLUSH)` (currently 5s).
Trace-provider shutdown and the final log drain share D; there is no new stage
ceiling or grace-tail addition. Retain tracer-provider implementation and its
500ms join-slack policy, but bound the caller's await by D so that slack cannot
extend this shared stage. A dropped wait does not cancel the started exporter
closure; existing runtime-shutdown policy remains responsible for its residual
execution. Publish D to the outer entry owner. No provider helper is rewritten.

On startup failure before a regular shutdown plan, establish the same configured
grace deadline when cleanup begins. Worker `abort_startup` consumes that deadline
with its existing cleanup/listener/background/dependency stage ceilings, then
hands off the remaining telemetry-stage deadline. An error immediately after
subscriber installation with no resources to close has only the same 5s
telemetry ceiling, clamped to grace. Retain the original error as primary; this
change does not add startup-resource or tracer-provider cleanup beyond that
needed for logging custody.

After async serving/cleanup returns, the outer entry records any terminal
service/worker failure through the still-live subscriber. A pre-install error
keeps `process_failure` stderr handling; after installation do not duplicate the
terminal error synchronously to stderr. Close/drain logs by D, then call the
existing `Runtime::shutdown_timeout(1s)` and map the final exit. Thus the existing
staged grace budget and separate 1s runtime shutdown allowance do not increase.
Residual task logs after admission closes count as `stopped`; they are not
intended terminal records and cannot reopen output custody.

The current `shutdown_completed` event is emitted before its own delivery is
known. Retain its event name and existing stage outcome, and add
`logging.flush = "pending"`; document that `outcome` describes preceding stages
and the process exit is the final composite result. `telemetry_flushed` refers
to the tracer only. Never emit `logging.flush=complete` before confirmation or
pretend the last line can acknowledge its own delivery. If logging shutdown
is Failed or TimedOut, otherwise-graceful service/worker exit becomes 3.
Primary startup/runtime errors stay exit 1 and usage stays exit 2. Prior drops
alone leave otherwise-successful exit unchanged.

Migration retains its existing order: apply finishes, runtime teardown occurs,
then `migration_run` records the known DB result. Start a terminal deadline when
apply returns using the existing 1s `RUNTIME_SHUTDOWN_TIMEOUT`; runtime teardown
and final log drain share it. Shutdown uses remaining time, then emit the same
terminal record and drain with remaining time. If runtime teardown consumes the
allowance, immediately close and report incomplete unless completion is already
known; add no new second. Runtime-build failure after subscriber installation
uses the same existing 1s terminal allowance. Logging failure maps successful
migration to exit 1, without claiming rollback or changing migration result
fields; primary migration/config/signal errors keep precedence.

Every production install caller is covered: service bootstrap/entry, jobs-worker
bootstrap/entry and migration main. The existing telemetry panic-hook fixture
uses a local in-memory subscriber; it obtains no production guard and needs no
custody change. No new public REST/OpenAPI,
database, profile selection, configuration or thread-sizing behavior is added.

## Business-work guidance

The runtime/contributor authority is [Runtime Lifecycle](../../../docs/architecture/runtime-lifecycle.md):
add one section distinguishing short bounded sync work, blocking I/O, sustained
CPU work and immediately-ready async loops. Name that file in `.agents/skills/rust-tokio` prose while retaining its actionable
admission/lifetime rule; the decision-skill format forbids Markdown links. Update
that skill's current suggestion to merely let the caller enforce a deadline:
admit before submission, move the permit into actual execution, retain completion/
panic observation beyond waiter timeout, request cooperative cancellation, and
account for execution still alive at shutdown. Existing job-handler cancellation
and effect/fencing guidance links to this owner; no new job policy is invented.
A separately bounded CPU executor requires a concrete later workload decision.

Correct comments/docs claiming runtime timeout kills blocking work, and the
stale unconditional available-parallelism description. Runtime timeout stops
waiting; running blocking closures and stalled stdout can survive until process
exit. Typed worker override and unset default remain current code authority.
Update logging/lifecycle docs for best-effort buffering, counters, terminal
result and migration success-with-log-failure semantics. Preserve current skill
carrier generation; instruction changes are static proof, not measured agent
improvement. Boundary case: timed-out waiter does not release admission while
closure runs. Retention case: bounded cheap computation stays inline.

## Proof and reopen boundaries

Feasible local falsifiers live beside the adapter/writer and existing binary
process tests: finite and endless empty-frame sources, controlled wakers,
saturated/stalled/failing writers, terminal records, loss snapshots, and exit
precedence. Executors choose concrete cases and commands during implementation.
Existing JSON correlation/filtering and upload integrity coverage remains the
compatibility authority. No new environment, benchmark campaign, or mandatory
DB experiment follows from this design. Final assembled concurrency review
covers interactions and all consumer custody. Reopen Design for an unsupported
API or budget/owner gap, and Specification only if resolving it changes behavior.
