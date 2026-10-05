# Runtime progress under one CPU quota

This proof qualifies one finite workload against the real service process. It
does not establish a fleet SLO or preemption of arbitrary callbacks. The
workload and numerical outcome criteria below are fixed before measurements;
source inspection, compilation and daemon capability do not establish a
successful result.

The [service example](../crates/service/examples/runtime_progress.rs) registers
the existing generated Echo contract through `service::run_with_grpc`. It uses
the production bootstrap, runtime, allocator, request admission and timeout,
listeners, telemetry and teardown. The
[external process driver](../crates/service/tests/runtime_progress.rs) owns
offered traffic, provider peers, observations and bounded cleanup. The shipped
service binary does not register this workload, and the fixture does not copy
a runtime or shutdown implementation or substitute a standalone timer server.

Provider-specific real database and broker obligations remain with their
existing proof owners. The ordinary process and readiness cases independently
own stopping during active or cancelled startup/work and stale-failure
evidence showing no false readiness revival. Trace completion is local export
completion, not receiver durability or proof of a final scrape.

## Finite computation and completion custody

The [executable CPU recipe](backend-utility-recipes.md#bounded-cpu-work-with-completion-ownership)
accepts at most 64 KiB, 4096 fixed-width items and 4096 rounds: at most
16,777,216 item operations. It retains at most 64 KiB of result. There is one
admitted CPU operation and no waiting admission queue. Input bounds and
`Semaphore::try_acquire_owned` precede copying, decoding and `spawn_blocking`.
The actual blocking closure owns the permit until its work ends; dropping the
separate result receiver cannot release capacity or admit replacement work.

The feature owns a `TaskTracker` and one process-lifetime manager registered
through the service's supported `BackgroundRegistration`. Submission and
closing admission/tracking share one gate with no await. A tracked supervisor
owns each blocking join handle, including after waiter cancellation. On root
cancellation the manager closes admission/tracking and joins. On actual work
failure it closes admission/tracking, reports immediately through
`BackgroundFailureReporter`, and remains in the root task set until every
admitted supervisor has retired before returning an error. A failure during
that join is still reported and retained. Reporting is not completion.

The existing parent shutdown deadline and exit mapping remain authoritative.
Started blocking work cannot be aborted; an expired wait is incomplete and
keeps consuming its actual capacity until completion or process exit. No new
shutdown budget or retry of an unknown business effect follows. A real feature
supplies its own useful computation, validation/error owner, cost evidence and
lifecycle registration before changing the specimen's limits; this example
adds no shipped CPU service, generic pool, configuration key or public business
endpoint.

## Fixed quota workload and outcome criteria

Use one Linux release build with the accepted lockfile and normal runtime,
allocator and feature selection. The example supplies cheap successful Echo
requests, the bounded CPU specimen and real adopted upload, logging and
serialization calls. Run the service container at 100000 us quota / 100000 us
period (1 CPU), explicit `runtime.worker_threads=1`, one CPU permit and no
queue. Load generation and stub peers run outside that quota. Read `cpu.max`,
effective workers, `cpu.stat` before/after and image/source identity. Under
sustained mixed pressure, `nr_throttled` and throttled time must increase;
otherwise pressure/enforcement has not been demonstrated. Docker host NCPU is
never used for sizing.

| Dimension | Fixed envelope |
| --- | --- |
| Cheap traffic | 1 KiB successful unary Echo, open-loop 100 requests/s, at most 32 outstanding, 8 s request timeout; errors/timeouts/dropped offers counted separately. Probe routes do not substitute for this traffic. |
| Capacity precondition | Same image/quota, cheap-only 200 requests/s for 30 s: at least 99% offered requests succeed, p99 at most 200 ms, maximum completion gap under 1 s. If unmet, this envelope is not qualified; retain the result and revisit sizing before a changed experiment. |
| Compared baseline | Cheap-only 100/s for 30 s, after 5 s warmup. Preserve the complete attempted/offered/completed timeline and baseline percentiles. |
| Mixed pressure | 30 s cheap traffic plus 40 heavy attempts/s, one admitted CPU operation at a time, finite specimen above; refusals recorded. At least 10 CPU operations complete and admission refusal is observed. |
| Combined sources | Alongside CPU work, finite 128 KiB uploads at 4/s, each with no more than 4096 empty frames before its data, maximum 2 concurrent; real bounded generic preparation of trusted primitive payloads no larger than the admitted job limit; 4096-byte known log strings at 1000 attempts/s with max 32 concurrent callbacks, while stdout is deliberately not drained for 10 s, then resumed. No unbounded frame/generator or preparation fan-out. |
| Observation | External ready/live/metrics observations every 100 ms; capture failed/late scrapes too. Internal sampler stays 100 ms. Readiness defaults remain 2 s cadence, 4 s probe budget, 16 s stale bound; default drain/grace remain 25/45 s. |
| Waiter cancellation | Cancel one already-started admitted CPU wait while the closure is still active; demonstrate active permit remains held and another attempt refuses until actual completion. Observing only the waiter return fails this requirement. |
| Release/recovery | End heavy offers and resume stdout; observe actual residual completion, fresh sampler/readiness and baseline-like cheap progress for 10 s. Stop only after this recorded recovery for the ordinary graceful case; independently retain source lifecycle proof for stop during active/cancelled startup/work. |
| Repetition | Three baseline/mixed/recovery sequences, serial, same build/settings. Retain all runs and adverse samples; no best-run selection. |

Every mixed run must meet all of these:

- Cheap success goodput at least 95% of its comparable baseline and at least
  95/s; p99 at most `max(500 ms, 4 * baseline p99)` and never above 2 s.
  Maximum gap between successful cheap completions is below 2 s, including
  window edges. Any full configured 8 s timeout interval without a success is
  an unconditional qualification failure, regardless of eventual recovery.
- Internal timer lag p99 at most 500 ms and maximum below 2 s; report sample
  count and scrape-time age with external gaps. External live/metrics/ready
  response gaps below 2 s during supported admitted pressure. Ready stays
  truthful: the separate stale-failure evidence must show no false revival.
- CPU active work never exceeds one, refuses excess, stays nonzero after the
  cancelled waiter until actual completion, then reaches zero. Each finite
  specimen completes within 1 s in this envelope. Uploads remain capped at two
  and complete/cancel without a polling monopoly; log saturation produces
  counted drops, no application sink wait, and later valid records recover.
- Within 10 s of ending pressure, all finite residual work is accounted for,
  sampler age is at most 1 s on successful scrapes and cheap success rate is
  at least 99/s with p99 at most `max(200 ms, 2 * baseline p99)` for the final
  five seconds. A frozen old zero/Ready sample does not satisfy recovery.
- A subsequent healthy sink/exporter stop returns 0 within the single 45 s
  grace plus at most 1 s declared external signal/measurement tolerance.
  A voting blocked/failing final output/trace case returns 3 within the same
  bound; a primary failure stays 1. Tolerance does not extend internal stage
  deadlines or permit hidden work to be declared complete.

A bounded negative control runs finite uncooperative work on the sole Tokio
worker for longer than the 8 s request budget, with an external 12 s cutoff.
It must produce the expected cheap-request gap and a missed timer observation
on resumption; it proves that the observation detects starvation, not that the
workload passes. Its total iterations/time are finite and it is joined or its
owned fixture process is stopped afterward. Equivalent existing whole-process
evidence on the same observation may replace repeating this control.

## Observation and retained evidence

The production sampler first targets start + 100 ms, records non-negative
monotonic lateness on actual completion, and sets the next target to 100 ms
after completion without a catch-up burst. The histogram
`runtime_scheduler_lag_seconds` retains resumed-gap observations with buckets
0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2, 4 and 8 seconds.
`runtime_scheduler_samples_total` starts at zero. At render,
`runtime_scheduler_sample_age_seconds` reports monotonic last-completion age,
NaN until the first sample; it keeps aging if the sampler freezes while the
metrics handler can execute. `runtime_scheduler_freshness_limit_seconds` is
fixed at 1 second and is an observation threshold, not a readiness/restart
policy.

`runtime_scheduler_lag_max_seconds` is the cumulative largest actual observed
lateness, NaN until the first sample, retained through later timely samples.
It proves the strict `maximum < 2 s` criterion: the histogram's inclusive
`le=2` bucket cannot establish that strict boundary or an exact maximum. Read
a fresh final positive-run snapshot after at least one resumed sample and the
end of the workload. A missing or stale terminal observation leaves proof
incomplete. Reusing one process retains the same strict maximum across all
three positive sequences. The deliberately starving negative control uses a
separate process so its expected large maximum cannot contaminate them.
Histogram differences still supply percentile evidence; do not strengthen the
threshold to 1 s or relax it to `<= 2 s` to fit a bucket.

Samples may change while separate metrics are emitted. Interpret increasing
sample count, age, successful scrapes and external monotonic timestamps
together. A missing scrape, startup NaN/zero count, disabled diagnostics or
stale last sample means unknown progress. The private diagnostics listener
separates connection capacity, not CPU/scheduler capacity. Readiness keeps its
own completion/freshness signals and decision clocks.

Retain exact source/archive, image, binary and toolchain identities, effective
workers, quota/period and `cpu.stat` before and after. Preserve every offered,
attempted, completed, failed and dropped request, dispatch lateness, goodput,
percentiles and successful-completion gaps including window edges. Keep the
full successful/failed/late scrapes, internal sample count/age/lag, external
probes, CPU admission/refusal/cancellation/actual completion, upload outcomes,
log drops/recovery, residual completion, process exit codes and grace duration.
Report all three runs and all adverse samples; never select only a passing
repeat. Source inspection, a successful build, an ignored test, or an earlier
receipt against different inputs cannot replace this execution.

## Commands and fixture inputs

On a Linux host with Docker cgroup v2 and the full retained fixture profiles,
the [canonical runner](../scripts/ci/runtime-progress-proof.sh) builds the
release example once, records source/archive/binary/toolchain identity, wraps
that same binary with the [proof image recipe](../build/docker/runtime-progress.Dockerfile),
and invokes the existing ignored driver once:

```sh
ALLOW_HEAVY=1 make runtime-progress-proof
```

Set `RUNTIME_PROGRESS_RESULTS` to a new absolute directory to choose the result
location; the runner otherwise uses its own ignored `.artifacts` subtree.
Existing result directories are not overwritten. The runner owns its build
and execution commands; do not duplicate the build for each sequence or
profile. Reuse the one release image and existing dependency/build caches.
The runner retains input identities beside the results in
`<RUNTIME_PROGRESS_RESULTS>.inputs`; CI uploads the parent
`.artifacts/runtime-progress` directory on success or failure for seven days.

The underlying ignored test is
`bounded_sources_preserve_process_progress_under_one_cpu_quota` in the service
process driver. Its explicit inputs are `RUNTIME_PROGRESS_IMAGE`,
`RUNTIME_PROGRESS_SOURCE` and a new absolute `RUNTIME_PROGRESS_RESULTS` path.
The image's `org.opencontainers.image.revision` must equal the supplied source
identity. The driver pins all instances to the inspected image ID and records
the executable SHA-256. A manually supplied image must contain
`/proof/runtime_progress`, `/bin/sh`, `cat`, `mkfifo` and `sha256sum`. Docker must
route `host.docker.internal:host-gateway` to the external driver peers.

The fixture-local `RUNTIME_PROGRESS_UPLOAD_ENDPOINT` names the finite external
S3 peer. Echo metadata `x-runtime-work` selects `cpu`, `upload`, `prepare`,
`log`, `snapshot` or `freeze`; without it, Unary echoes exactly 1024 bytes.
The upload calls the adopted `ObjectStorage::put`/`PutBody::stream` path, and
preparation calls `infra_messaging::PreparedEvent::prepare` with a trusted
primitive payload bounded by 262144 bytes. These calls require no live
database or broker. Actual stdout backpressure pauses a FIFO reader for 10 s
and resumes it; merely leaving `docker logs` unread would not stop the daemon
from draining application stdout. The finite negative control deliberately
occupies the sole worker for 9 s and its separate process is killed at the
external 12 s cutoff and waited; graceful exit 0 is proved by the recovered
ordinary process.

The derived `runtime-progress` profile is retained only with gRPC, OIDC JWT,
messaging and object storage. Removing any prerequisite removes the example,
driver, runner, proof image recipe, this guide and fixture-only dependency and
documentation markers. It adds no user-facing profile selector or production
Cargo feature.

Run locally with the verified daemon or in existing CI when local disk cannot
hold a Linux release build. If the environment cannot produce the required
execution, report the runtime proof as pending and the larger qualification
as incomplete. A numerical failure requires a causal decision about the
accepted envelope before a changed experiment; it does not authorize weaker
thresholds after seeing results.
