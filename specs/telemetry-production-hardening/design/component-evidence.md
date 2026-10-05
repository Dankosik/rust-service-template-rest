# Bounded output component decision

Evidence date: 2026-10-05. This is the narrow dependency/mechanism comparison
required by Technical Design, supplementing the accepted research.

| Candidate | Verified behavior, cost and disposition |
| --- | --- |
| Existing synchronous JSON and tracing-subscriber text | Reuses semantic owners, but both perform caller-side writes and grow formatting buffers without the required bound. Retain JSON semantics, replace output and bound capture. |
| `tracing-appender` 0.2.5 | Latest registry version, published 2026-04-17 13:23:16 UTC; MIT, MSRV 1.63, maintained in tokio-rs/tracing. Its documented lossy bounded queue and `MakeWriter` integration solve off-thread output, but not byte admission, bounded scratch/span storage or outcome-bearing deadline cleanup. Do not admit. |
| Appender plus writer/guard adapters | A writer adapter could count write/flush errors, and a bounded formatter could cap individual messages. `WorkerGuard` still has no explicit result or caller deadline, waits 100ms to enqueue control plus 1000ms for acknowledgment, synchronously prints on full control queue, and never joins its thread. Worker ignores normal write failures and prints final flush failure to stderr. A helper thread would merely hide these paths and need a second completion protocol. That is more machinery than the selected single worker. |
| Standard bounded channel + one OS writer | Selected. `try_send` supplies non-waiting full admission, FIFO queueing and a finite slot count. Application-owned complete-record cap, admission closure, error counters and separate one-slot completion channel close the exact missing obligations. `recv_timeout` permits bounded cleanup when Tokio construction failed or its runtime has already stopped; async consumers put only that bounded control wait on existing `spawn_blocking`. Standard channel/thread internals stay standard library mechanisms; no custom queue or scheduler. |

Primary authorities: [registry metadata](https://crates.io/api/v1/crates/tracing-appender),
[0.2.5 release history](https://github.com/tokio-rs/tracing/blob/tracing-appender-0.2.5/tracing-appender/CHANGELOG.md),
and the installed matching source under
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tracing-appender-0.2.5/src/`:
`non_blocking.rs` (`Write`, `NonBlockingBuilder`, `WorkerGuard::drop`) and
`worker.rs` (`work`, `worker_thread`). These source limitations are the decisive
known issues; no unperformed advisory audit is claimed for a rejected crate.
An added crate would require current advisory/license/resolution evidence; none
is added by this choice. Reopen if an upstream supported API supplies all the
deadline, observable completion, no-fallback and bounded-formatting contracts
with less adapter code.

The existing resolved OTel 0.33 sources were reopened only for the decisions
they affect: `opentelemetry_sdk/src/trace/span_processor.rs` final shutdown and
queue-drop events, `opentelemetry-otlp/src/exporter/http/{mod,trace}.rs` raw URL,
body, error-message and partial-success paths, and
`opentelemetry/src/global/internal_logging.rs` static metadata names and package
targets. Raw suppression plus numeric projection uses tracing-subscriber 0.3.23
global Layer `register_callsite`/`enabled`/`event_enabled`, never a formatter-only
redaction or OTLP response parser. Installed source and current lockfile are the
API authority; changes to these versions reopen their affected assumptions.

The review's proposed unconditional stdout exit-deadlock falsifier was rejected
by exact [Rust 1.99 stdio source](https://raw.githubusercontent.com/rust-lang/rust/1.99.0/library/std/src/io/stdio.rs)
and [LineWriter source](https://raw.githubusercontent.com/rust-lang/rust/1.99.0/library/alloc/src/io/buffered/linewritershim.rs):
exit cleanup uses `try_lock`; complete-line `write_all` from an empty line buffer
writes directly. The selected worker preserves that framing, performs explicit
final flush, and adds no application buffering or fallback writes. The finite
standard buffer is accounted as 1 KiB library overhead. This does not establish
the same property for arbitrary other stdout users. A duplicated stdout handle
is technically available through safe standard APIs but is not needed for the
verified selected path; no speculative duplicate-handle adapter is added.
