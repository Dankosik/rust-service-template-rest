# Telemetry hardening: Technical Design

Status: ready

Authority: [ready Specification](../spec.md), SHA256
`d3bb54400a9f9ffb9634d2653ddbeb64ce179dea1700ad622c66fe747470e8f2`,
and [accepted evidence](../research/accepted-evidence.md). Source baseline:
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. This result selects mechanisms and
ownership, not implementation or measured performance.

## Selected mechanism and dependencies

Keep the stock OTel 0.33 serial batch processor, HTTP/protobuf exporter,
compression, retries, propagation, sampling, registry and histogram buckets.
Extend `Counted` to retain final-drain failure evidence. Put one bounded local
formatter and one owned OS writer thread behind the existing subscriber.
JSON and text share bounded field capture and admission. All privacy admission
precedes the local and OTel layers.

No new crate, feature or version is admitted. Use Rust 1.99
`std::sync::mpsc::sync_channel` and `std::thread` for the writer and bounded
completion wait, and the current tracing/serde APIs for bounded formatting.
Existing Tokio 1.53.1 `spawn_blocking` keeps the bounded control wait off async
workers; completion itself needs no runtime. The
[component comparison](component-evidence.md) records why the supported
appender extension points do not close this lifecycle and why the small adapter
is necessary. No fork, custom batch processor or parallel logger pipeline.

The existing JSON formatter supplies its unique/reserved-key, span precedence,
normalization, timestamp and top-level correlation semantics. A writer wrapper
alone cannot bound its current growable formatting/storage or the text layer's
intermediate String. Replace both local format paths with one bounded layer;
retain the JSON algorithm and render a human-readable text line from the same
admitted fields. Text keeps level, time, message, span context and correlation;
byte-for-byte tracing-subscriber text layout and ANSI styling are not contracts.

## Local logging bounds

These are code-owned ceilings, with no new configuration keys. A byte means an
octet in encoded local output or owned serialized field storage, as specified.

| Resource | Ceiling and accounting |
| --- | --- |
| Encoded record | 16 KiB including the final newline, for either format |
| Record field metadata | 128 entries, including flattened span fields before duplicate elimination |
| Per-callback scratch | one 16 KiB output buffer, one 16 KiB serialized-value buffer and at most 128 field entries; no growable fallback or retained thread-local byte buffers |
| Concurrent local formatting callbacks | 32 non-waiting permits process-wide; an unavailable permit drops local work with `busy` |
| Cached local fields per span | 4 KiB serialized values and 64 field entries, including its name |
| Cached local span contexts | 1024 occupied slots process-wide, reserved before allocation and released on final span close |
| Writer queue | 512 complete records; at most 8 MiB encoded payload, plus bounded slot metadata |
| Writer in progress | one complete record, at most 16 KiB; no second batching buffer |
| Writer threads | exactly one per successful subscriber installation; never restart or replace a blocked writer |

The payload bound is queue 8 MiB + writer 16 KiB + live scratch 1 MiB + cached
span values 4 MiB. Field-table storage is additionally bounded by
`32 * 128 + 1024 * 64` entries (each holds a static key reference and byte
range); channel/Arc/slot overhead is finite and recorded separately from payload.
Allocation capacity, including compaction/replacement scratch, must fit this
accounting, not merely logical length. Move the completed owned output buffer
into the queue; do not allocate a third full-record copy beside the two scratch
buffers. Fixed-capacity backing storage makes
oversized input unable to grow a Vec/String before rejection. Span replacement
uses the callback scratch, not an additional unaccounted full copy.

16 KiB leaves substantial headroom over the present request/access and lifecycle
records (bounded 128-character request id and a small finite field set) without
making a stopped sink retain the appender default's 128,000 arbitrary records.
512 slots absorb bursts, not a time guarantee. 1024 cached spans give four
contexts per default 256 HTTP in-flight requests; saturation remains allowed
with deeper trees, background work or larger configured concurrency. These are
engineering limits, not a capacity measurement or a promise of loss-free load.
Reopen this design if current normal-record fixtures exceed them; do not silently
truncate, add knobs, or enlarge a limit during implementation.

All writes into owned serialization buffers stop at their limit, including
`serde::Serializer::collect_str`/Debug/Display streaming and escaping expansion.
Do not use `format!`, `to_string`, an unlimited serde Value, or a complete
temporary String in these capture paths. A source-owned Debug/Display may do
unbounded work or allocate before writing; that behavior and caller-owned input,
static callsite metadata, the tracing registry, SDK span storage, allocator/OS
overhead and unrelated metrics are outside this logger-owned bound. Both Debug
and Display are expressly covered by this limitation. This is not an RSS cap.

An event/value/field-count overflow drops the whole local record (`oversize`).
A local span capture that exceeds its size/field/slot budget is unavailable
for its lifetime (absence of cached local fields, without an allocated sentinel
or retaining the rejected value). Events whose selected
scope includes that span drop as `span_capacity`; they do not silently lose
context. Span close releases its slot. Local overload does not disable the
OTel layer or alter its sampling. Surviving JSON is a complete valid line with
the existing key and correlation semantics; no partial line is queued.

A thread-local boolean rejects reentrant local formatting (`reentrant`) before
any scratch allocation or span-extension access. Span mutation first captures
the incoming field delta in bounded scratch outside extension locks, then merges
serialized values under the short extension lock. Event formatting copies
already-serialized context under the lock, releases it, then performs formatting.
No user Debug/Display, tracing call, sink write, or worker wait executes while
holding a span-extension lock. The worker, loss counters and cleanup never log
recursively. Reentrant source formatting can still affect unrelated third-party
layers; this adapter does not promise to bound arbitrary user code in the SDK.

## Writer ownership and terminal observation

The layer's sender submits one owned complete record with `try_send`. Full
queues drop the new record (`queue_full`) with no synchronous fallback and no
retry; admitted order is channel FIFO. Admission never waits for sink capacity.
The worker owns stdout and all write/flush calls. Production output submits only
whole newline-terminated records using `write_all`, with no application-side
BufWriter or extra partial-line writes. Rust 1.99's full-line path from an empty
LineWriter writes directly, and exit cleanup uses `try_lock`; a held writer lock
therefore does not force an exit wait. The standard LineWriter's finite 1 KiB
capacity is additional library overhead, not queue payload. No direct stdout
write may be introduced on a post-install cleanup path. This claim remains
subject to the blocked-pipe process proof and reopens if source/sink semantics
change; arbitrary external stdout writers are outside the logger guarantee.
It attempts each queued record
once, counts write failures, then accepts later records so a resumed sink can
recover. An error after a partial OS write is `write_error`, never delivery.
Flush failures are counted separately; idle/final flush occurs on the worker.

`install_subscriber` returns a must-use `LoggerGuard`; its public explicit
`shutdown(deadline: std::time::Instant)` is a synchronous, runtime-independent
bounded control wait, returning `Completed` or `Incomplete` with finite reason
bits and a local loss snapshot. It closes admission, then uses a separate
one-slot standard completion channel and `recv_timeout(remaining)`; the worker
uses `try_send` after its terminal cleanup. There is no sink I/O in this method.
Service/worker call it through their existing `spawn_blocking` mechanism and
bound that job by the same absolute tail deadline. On expiry, the wait job
itself returns by that deadline, even when the writer remains blocked; its Drop
cannot wait again. Migrate calls the same method directly, including when no
Tokio runtime exists. Thread creation failure is a typed installation
failure. Installation failure after thread creation closes its admission and
detaches without waiting or printing. `Drop` only closes admission and drops
the JoinHandle; it performs no I/O, wait, panic, logging, or second drain.

Close is a one-way admission state change. Callbacks that started earlier must
finish or be rejected at their submit boundary; a worker cannot acknowledge
completion while a publisher can still commit. A small admission critical
section contains only closed-state checking and `try_send`, never formatting or
I/O. The control path closes it without waiting on sink work. Queue-empty plus
closed admission permits final flush. Use a separate completion channel, so a
full data queue cannot swallow shutdown control. A bounded receive timeout of
at most 10 ms allows an idle worker to notice closed admission without another
control record occupying the data queue.

Completion is sent only after the worker's last sink operation and resource
cleanup. An acknowledgment alone does not justify an unbounded join: perform a
join only after `JoinHandle::is_finished`, inside the same caller deadline;
use a bounded control wait with at most 1 ms polling while waiting for that
terminal thread state, then detach and classify incomplete if time expires.
A forever-blocked write keeps that one
OS thread and bounded retained queue alive until process exit. It is not a Tokio
blocking task and cannot make runtime destruction wait forever. No replacement
thread, reconnect queue, disk spool or attempt to kill an OS thread is added.

At the start of the telemetry tail, take a logger final-drain observation marker.
Any write/flush failure completing after that marker, including an already
in-flight write, or any final-record admission loss makes its result incomplete.
Earlier runtime losses remain in counters but do not alone degrade a later clean
shutdown. A deadline expiration/panic/disconnection is always incomplete.

Local atomics own accounting independently of output: publish snapshots through
the existing metrics upkeep owner. Use `telemetry_log_records_dropped_total`
with only `queue_full|oversize|span_capacity|busy|reentrant|closed`, and
`telemetry_log_sink_errors_total` with only `write|flush|worker`. Registration and
absolute/delta publication must not lose pre-recorder observations or double
count repeated upkeep. Guard snapshots exist even without a recorder. No worker
error text or input value becomes a label. The final logger verdict may be
visible only in the exit outcome: logging that verdict after closing the logger
would itself require a new unproved drain.

## Trace completion evidence

`Counted` and `TracerProviderHandle` share a small observation state. Beginning
shutdown and recording export completion are serialized by a short lock with
no awaits, I/O, arbitrary formatting or tracing under it. The begin operation
sets a one-way draining flag; an export finishing afterward latches its finite
failure class (`timeout|already_shutdown|internal_failure`) until the result is
consumed. This includes an export started earlier. An earlier completed runtime
failure is historical and cannot poison a later drain. Every failure during the
drain survives subsequent successes. The wrapper also records a failing inner
`shutdown_with_timeout`, which SDK outer success otherwise can hide.

`ProviderShutdown::Completed` replaces `Flushed`: the provider call and its join
completed and no final-drain failure was observed. All other paths yield
`Incomplete` with finite reason bits, including provider error, join failure or
deadline. Keep raw error values out of tracing and returned Debug/Display
surfaces. The existing exported-span metric keeps its series but describes the
number of spans in batches whose SDK exporter returned success/failure. It is
not accepted/rejected/persisted spans. Partial-success and malformed-response
diagnostics do not change the Counted result into an invented protocol verdict.

## Source privacy and SDK diagnostic admission

HTTP `observe.rs` stops collecting raw path/query, User-Agent, host/address/port
and removes the query denylist. Route or `<unmatched>`, normalized method (also
in span name and access event), finite result/protocol, timing and admitted
correlation remain. gRPC `make_span` conditions host/port/User-Agent collection
on the client role: server never collects them, client destination semantics stay.
Neither creates a second sanitized span after first recording an unsafe one.

The shared panic hook always withholds payload and arbitrary thread names and
backtraces. Keep `panicked`, source file/line/column and bounded current span
correlation. Remove the `PanicMessage` option and all caller choices. HTTP
recovery records a stable `http_handler_panicked` category without payload and
continues returning the current sanitized 500 Problem. Source locations are
build-authored; no free-form runtime identity is treated as a safe category.

Extend the current global INFO-span/event filter with a mandatory diagnostic
admission policy ahead of **both** output layers, at every configured log level.
For `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
`opentelemetry-http` (their module/hyphen/underscore target forms), and
`tracing_opentelemetry`, raw diagnostic events and spans never pass. Permit only
recognized SDK event metadata to reach `event_enabled`; its visitor records
numeric fields and finite metadata categories into local atomics, returns false,
and never invokes arbitrary Debug or re-emits a tracing event. Integrate this
with the existing global level filter so its known loss events can be observed
even when the normal event level suppresses them. Preserve ordinary INFO spans,
Targets/EnvFilter semantics and the existing AWS SDK log cap.

The exporter transport's `reqwest`, `hyper`, `hyper_util`, `h2`, `rustls` and
`rustls_platform_verifier` target families are denied raw diagnostics too.
Their background threads do not reliably carry exporter context, so this target
rule is process-wide and also suppresses their low-level diagnostics for other
users; provider-owned finite application records remain available. No raw debug
escape is introduced. Bridged `log` events are classified by normalized origin
target, not the generic `log` callsite; unknown fields and unknown event names
in denied families stay withheld. This is a concrete library boundary, not an
attempt to classify every future application string.

Minimum admitted SDK facts from the resolved source:

| SDK event | Safe observation (never raw text) |
| --- | --- |
| `BatchSpanProcessor.SpanDroppingStarted` | finite `queue_dropping_started` occurrence |
| `BatchSpanProcessor.SpansDropped` | latest cumulative numeric `dropped_span_count`, not repeated increments |
| `HttpTraceClient.PartialSuccess` | finite `partial_success` occurrence and non-negative numeric `rejected_spans` when present |
| `HttpTraceClient.ResponseParseError` | finite `response_parse_error` occurrence; no error formatting |
| `HttpClient.StatusError` | finite `http_status_error` occurrence; numeric status only |
| `HttpClient.NetworkError`, `HttpClient.ResponseBodyTooLarge`, `BatchSpanProcessor.ExportError` | corresponding finite occurrence |

Use `telemetry_sdk_diagnostics_total{event=<closed catalog>}`;
`telemetry_sdk_queue_dropped_spans` is an observed cumulative gauge, absent until
reported; `telemetry_sdk_reported_rejected_spans_total` adds only explicit
positive SDK reports. Numeric absence is unknown, never zero rejection. These
diagnostic metrics complement Counted, do not inspect OTLP responses or prove
delivery, and are unavailable to the operator without a successful scrape.
Additional numeric statuses may remain in the guard snapshot; no status label
or raw text is required. Exporter construction/shutdown records owned by this
crate also use finite categories (`exporter_build`, SDK error classes, join and
deadline); truncating arbitrary error text is removed as a privacy mechanism.

## Consumers, budgets and completion

Public changes are the returned `LoggerGuard`, its deadline-based shutdown and
snapshot, `ProviderShutdown::Completed`, finite incomplete reasons, and a
payload-free panic-hook installer. No general lifecycle framework is added.

| Consumer | Owner and terminal mapping |
| --- | --- |
| Service normal/signal exit | bootstrap owns both guards immediately after acquisition; normal staged teardown, then trace and logger tail. Any final incomplete result votes 3; an existing primary error wins with 1. |
| Service failed/interrupted startup | opened-resource slots exist before every fallible post-acquisition step, including subscriber/metrics installation; acquired resources use bounded cleanup. Failure is 1; interruption uses the existing 0/3 teardown mapping. |
| Ordinary jobs worker | resources retain telemetry immediately, not only in successful `Prepared`; `abort_startup` closes acquired tracer/logger after dependencies. Normal/signal 0/3, worker/business/startup failure 1. |
| `migrate` | installs only local logging. Preserve exactly one business `migration_run` record and original 0/1 result, including committed success and interruption/failure. Drain result is a separate snapshot/metric/best-effort finite record and never changes primary result or retries the operation. |
| Worker operator commands; `openapi`; pre-telemetry CLI/config failures | no shared subscriber/provider is installed; existing output and exit contracts remain. No telemetry installation is added to these finite commands. |

Service and ordinary worker retain the **existing 5-second telemetry tail** and
17-second aggregate tail. On tail entry let `D = min(now + 5s, process_deadline)`.
Reserve the last min(1s, remaining) for logger closure; trace total wait is
`max(0, remaining - 1s)`, at most 4s. Existing 500ms SDK join slack is deducted
inside that trace allowance, never added after it. The SDK receives the trace
allowance minus that slack; if none remains, skip waiting and report incomplete.
Both timeout/join and logger close use their shared absolute deadline (convert
Tokio's monotonic deadline to its standard Instant for the blocking wait, without
starting another duration). A late
provider result cannot extend it. Default SDK batching/retry controls remain;
the process's final wait is an independent best-effort cap.

Move post-startup primary-failure reporting before logger closure and remove
the duplicate synchronous `process_failure` fallback **after** successful logger
installation. Acquisition failures before any logger exists keep the current
stderr admission path. Keep the guard owned until the entrypoint returns; its
later destructor never waits. The existing final runtime shutdown cap stays 1s,
clamped to the remaining process grace deadline; a detached logger never joins
that runtime. Unexpected tasks logging after closure drop as `closed` and make
no final-record claim. Failed-startup cleanup without a stop-triggered deadline
uses its existing stage ceilings, with a shared 5s telemetry deadline.

For migrate, its existing 1s runtime cleanup allowance becomes one absolute
cleanup deadline spanning runtime shutdown, the business terminal record and
explicit logger drain. Runtime construction failure after logging also closes
the logger within this 1s ceiling. The primary 0/1 result remains authoritative
even if there is no time to publish the separate incomplete snapshot. No new
platform grace or migration execution deadline is introduced.

Rename `telemetry_flushed` to `trace_shutdown_completed` with
`delivery_confirmed=false`; incomplete trace outcomes use
`trace_shutdown_incomplete` and finite reason bits. Rename the pre-logger final
process record to `shutdown_finishing` with `logger_pending=true` and the known
stage result. It cannot claim that the subsequent local drain succeeded.
The actual final logger outcome affects the typed return and exit mapping; a
final log/scrape is never promised after the output boundary closes.

## Proof and operating boundary

Implementation chooses cases and controls within the [ownership map](ownership.md).
Required falsifiers are real JSON/text output for oversize/duplicate/reserved
keys and reentrant Debug, constrained span capture/repeated update, a gated or
failing writer under concurrent requests and final cleanup, queue/byte plateau,
drop counters and FIFO recovery, and a child process that exits with stdout
undrained. No test may unblock the sink before observing the bounded caller or
process result it claims. Release any deliberate blocked worker after the
assertion in an in-process test and join it; the child process proves exit when
the OS operation cannot be reclaimed by the parent deadline.

Receiver fixtures cover failing then successful final batches, failure already
in flight at shutdown, inner shutdown failure, partial success with malicious
message, malformed success and HTTP failure body/credential-bearing URL. Inspect
local JSON, text and exported span attributes/events under debug/trace; use
sentinels for every omitted HTTP/gRPC/panic/SDK source. Preserve successful
HTTP/TLS export, correlation, INFO-span filtering and compression coverage.
SDK diagnostic loss metrics must be checked at their real subscriber boundary,
including generic bridged log targets and absent counts.

Existing service/worker process tests prove startup, interruption, final records,
exit precedence and blocked-pipe termination. Existing
`scripts/ci/migration-validate.sh` image/process proof keeps
committed/no-change/failure meaning; changing no database behavior does not
authorize a new database harness. Its existing CI-owned migration gate remains.

`docs/configuration-source-policy.md`, `docs/architecture/runtime-lifecycle.md`
and `docs/infra-telemetry-performance.md` own operating updates. Document all
limits, removed raw fields/events, byte accounting, runtime loss vs final
degradation, no scrape after diagnostics closure, SDK queue 2048/batch 512/5s
defaults and retry/timeout caveats, metrics cardinality/upkeep, parent sampling,
and Collector/backend/deployment responsibilities. Preserve historical benchmark
workloads and dates as historical evidence.

Prospective cost evidence uses the existing real formatter/correlation fixtures
and process owner: compare normal short records, maximum admitted records and
stopped/failing output, retain callback elapsed time/allocation observations and
retained-byte high-water values. If measured command comparisons are needed,
use the existing performance owner with `hyperfine --warmup 3`, fixed release
build/features/load and all samples; include one-request and saturated workloads
because prior off-thread output traded low-load cost for saturated throughput.
This is a reproducible diagnostic route, not a new benchmark project, remote
host, numeric speed gate, or claim of current improvement. Applicable build/test,
documentation, final review and CI boundaries remain repository-owned.

Reopen System Design for a violated bound/lifecycle/API assumption, Rust
Ownership for placement evidence, Research only for changed SDK/provider
behavior, and Specification for different privacy/loss/exit semantics. There is
no user-owned unresolved question.
