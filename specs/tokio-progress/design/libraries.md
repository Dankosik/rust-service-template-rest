# Output writer decision evidence

Evidence checked 2026-10-05. Scope is selecting the output adapter; no benchmark
or implementation run occurred. Resolved workspace: Rust 1.99.0, Tokio 1.53.1,
tracing-subscriber 0.3.23. No manifest, feature, lockfile or toolchain change is
selected. Local registry source was inspected at those exact versions; the
installed but undeclared tracing-appender source is 0.2.5.

| Candidate | Verified mechanism / constraint | Disposition |
| --- | --- | --- |
| Current layers plus synchronous stdout | Existing source writes each event directly on emitter thread | Fails stalled-sink progress. Reuse formatting and `MakeWriter`, replace output only. |
| tracing-appender 0.2.5 | Lossy finite queue and `MakeWriter` match admission. `ErrorCounter` counts failed sends, not sink failures. Guard drop waits fixed 100ms/1000ms, may print to stdout, and returns no result. Worker catches/discards I/O errors, acknowledges before its last flush, and may print that final error to stderr. | Reject for bounded, observable terminal drain. A sink wrapper can count errors but cannot change guard waits/printing or make its handshake a post-final-flush result; moving Drop to another thread leaves those semantics and extra custody machinery. |
| flexi_logger 0.31.10 | Maintained tracing integration exposes file writer setup; `WriteMode::AsyncWith` uses an unbounded channel. `pool_capa` bounds reusable message buffers, not queued records. Shutdown/flush APIs do not supply the required caller deadline and drop-newest admission policy. | Reject: retaining layer output via a writer adapter still requires replacing its queue and terminal-lifetime mechanism. File rotation/logger replacement is unneeded scope. |
| Existing Tokio 1.53.1 bounded mpsc plus a thread | `try_send`, receiver closure and blocking receive can build the policy; still needs explicit admission close, status and terminal receipt outside the runtime. | Viable, but standard channel already meets synchronous producer/thread-consumer boundary and needs no new feature use or async waiter. |
| Rust 1.99 standard bounded channel plus one thread | `try_send` distinguishes full/disconnected; sole sender drop permits ordered drain, `recv_timeout` bounds outer-thread waiting; dropping a join handle detaches. | Selected. Template owns only tracing output admission, failure evidence and process-budget integration; standard library owns queueing and thread machinery. |

The exact missing ready-made facility is an admission close that cannot wait
for queue space, coupled with a post-write/post-final-flush result and a
nonblocking destructor that performs no fallback output. No suitable supported
extension point among the compared maintained writers supplies all three.
The small `logging/output.rs` module exists for that current gap; removing it
would put policy in three binary roots or hide completion again. No generic
executor, logging framework, custom queue algorithm or custom thread pool.

Registry queries to crates.io on the evidence date returned tracing-appender
0.2.5 released 2026-04-17, tracing-subscriber 0.3.23 released 2026-03-13,
flexi_logger 0.31.10 released 2026-08-07; Tokio's latest was 1.53.2 released
2026-10-03, but the workspace remains 1.53.1. GitHub repository metadata showed
both tracing and flexi_logger unarchived, last pushes 2026-05-30 and 2026-08-06,
licenses MIT and Apache-2.0 respectively. No new dependency means no new supply
chain or feature-resolution acceptance; no claim of a comprehensive advisory
audit is made for rejected crates.

Sources: [tracing-appender source](https://docs.rs/tracing-appender/latest/src/tracing_appender/non_blocking.rs.html),
[tracing repository](https://github.com/tokio-rs/tracing),
[flexi_logger write modes](https://docs.rs/flexi_logger/latest/flexi_logger/enum.WriteMode.html),
[flexi_logger tracing integration](https://docs.rs/flexi_logger/latest/flexi_logger/trc/index.html),
[flexi_logger repository](https://github.com/emabee/flexi_logger),
[standard try_send](https://doc.rust-lang.org/std/sync/mpsc/struct.SyncSender.html#method.try_send),
[standard receive timeout](https://doc.rust-lang.org/std/sync/mpsc/struct.Receiver.html#method.recv_timeout),
[thread handle drop](https://doc.rust-lang.org/std/thread/struct.JoinHandle.html).
Local source additionally confirms tracing-appender `worker.rs` final-flush
ordering and subscriber 0.3.23 `fmt/fmt_layer.rs` one complete-event `write_all`.

Accept one thread, one owned copy per admitted event and short admission-mutex
contention. The historical [logging experiment](../../../docs/infra-telemetry-performance.md#2026-09-29-json-layer-static-filter-histogram-upkeep)
reported a low-concurrency regression for tracing-appender: this is real
tradeoff evidence, not a speedup claim or a new threshold. Reopen when a maintained
writer exposes the missing bounded-result API, or observed application workload
shows this capacity/copy/thread cost is unsuitable. Research that changed fork
before growing the template adapter.

Additional source checks: `metrics` 0.24.6 already provides `Counter::absolute`,
so scrape-time snapshots require no dependency change. Rust 1.99
[stdout cleanup](https://doc.rust-lang.org/src/std/io/stdio.rs.html#729) uses
`try_lock` at process exit, avoiding a lock wait on a writer stalled while
holding stdout. This supports the timeout-and-exit path; it does not prove any
collector received a record or authorize reliance on Drop for final flush.
