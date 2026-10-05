# Telemetry ownership map

Status: ready

This map refines [Technical Design](technical-design.md); its named sections
supply each row's constraints, cleanup, proof and reopen conditions. Crate
directions remain unchanged. No generated OpenAPI/protobuf source changes.

## Responsibilities

| Responsibility | Affected path / current evidence | Semantic owner and exact action | Boundary and cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- |
| L: bounded local capture | `logging.rs` installs JSON and stock text; `logging/json.rs` owns custom capture and output | `infra-telemetry`: rename `logging/json.rs` to `logging/format.rs`, extend it with bounded common fields and JSON/text rendering; retain the layer's correlation/key algorithms | Crate-private layer; no new public field model. Code-owned limits from Design. Local scratch/cached-span reservations are returned on all paths; no sink I/O here | Existing inline formatter and logging tests, moved with the owner | A required semantic cannot survive bounded capture; System Design |
| W: writer and loss state | synchronous local writers currently have no resource owner | `infra-telemetry`: add `logging/output.rs` for bounded record admission, single writer, atomic accounting, `LoggerGuard`, runtime-independent deadline closure | Existing std channel/thread/completion wait; async composition puts only the bounded control wait on existing Tokio spawn_blocking. No business/exit policy. Close/drop rules from Design; no unbounded guard destructor | Inline gated/failing-writer tests; process proof is C | A bound or cleanup cannot be established; System Design |
| D: raw SDK diagnostic admission | SDK 0.33 names/fields in component evidence; current global level and AWS SDK filters in `logging.rs` | `infra-telemetry`: add `logging/diagnostics.rs` for finite metadata policy, numeric-only visitor and observed SDK facts; compose with current global filter in `logging.rs` | Before both local and OTel layers; preserve INFO span handling/AWS cap. No protocol parser, raw fallback, recursive event, or library privacy API | Inline real-subscriber JSON/text/OTel tests and current delivery fixtures | Resolved library changes invalidate target/event coverage; Research then System Design |
| T: truthful trace drain | `traces.rs` Counted and provider handle currently lose drain failures and call completion Flushed | `infra-telemetry`: share drain observation state, replace outcome variant, sanitize owned initialization/shutdown diagnostics and metric help | Existing exporter trait extension, no processor fork. Lock only protects finite state; provider shutdown remains off runtime workers under caller deadline | `traces.rs` inline state tests and `traces/tests/delivery.rs` receiver fixtures | Required truth would need protocol/parser change; Specification |
| M: independent observed loss publication | `metrics.rs` owns installed recorder/upkeep | `infra-telemetry`: publish local W/D atomics through existing upkeep, initialize describes and finite labels | Same Prometheus pipeline, no total registry cap. Upkeep ends at existing task cancellation; snapshots remain usable after it ends | Existing metrics tests, W/D real-boundary assertions | New telemetry pipeline or changed histogram/cardinality policy needed; Specification |
| P: panic privacy | `logging.rs` shared hook, `infra-http/harden.rs` payload logging | Their current source owners remove payload/option/thread/backtrace emission and keep safe finite source/context | Hook shared by existing consumers only; HTTP status and Problem unchanged | Existing hook and hardening panic tests, plus C process records | Caller-visible recovery changes needed; Specification |
| H: inbound HTTP privacy | `infra-http/observe.rs` raw attrs, query denylist, unnormalized display method | `infra-http`: collect only accepted server fields and normalized method throughout span/access names | Current observation path; no exported privacy service or new dependency | Inline observer tests and existing service HTTP process/trace fixture | Route/method/status/contract meaning changes; Specification |
| G: inbound gRPC privacy | `infra-grpc/observe.rs::make_span` serves client and server roles | `infra-grpc`: omit authority/User-Agent only on server role | Same method-label and status contract; client identity preserved | Existing inline observer/client/server coverage, current gRPC process owner | Role separation cannot preserve client identity; System Design |
| C: resource ownership and exits | Service bootstrap+shutdown; worker prepare/abort_startup/run; migrate main | Each binary composition root acquires guards into owned optional slots immediately, funnels post-acquisition errors/interruption through cleanup, moves final failure records before close, consumes typed results | Same 5s tail/17s aggregate, 1s runtime cap and migration primary result; no infra crate exit policy. Full consumer table and budgets in Design | Service `tests/lifecycle.rs`, worker `tests/process.rs`, migrate main inline outcome proof and existing migration validator | Business result, startup admission, or platform grace must change; Specification |

Non-mechanical reuse dispositions: L extends existing source and current
serde/tracing APIs; stock text FormatFields/MakeWriter does not bound the whole
formatter, and parity proof preserves normal JSON keys, correlation and text
content. W uses the standard-library rung; tracing-appender 0.2.5 is the
strongest rejected source for the exact gaps in component evidence; parity is
lossy FIFO/recovery plus stricter deadline/outcome/byte proof. D uses the existing
global filter extension API at tracing-subscriber 0.3.23; formatter-only redaction
and transport-context-only filtering both miss another output/thread. T uses
the existing exporter wrapper extension at OTel 0.33; a new processor is
unnecessary. Upgrade/replacement condition for L/W/D/T is a supported upstream
mechanism that preserves all accepted constraints with less local machinery.

## Files

`L/W/D/T/M/P/H/G/C` reference the complete corresponding responsibility above,
including its cleanup/proof/error constraints. This is the inverse map; each
file has one current reason, even where one responsibility spans consumers.

| Path under repository root | Responsibilities and present reason | Declarations / visibility and call path | Lifecycle/error owner | Allowed dependencies / forbidden responsibilities |
| --- | --- | --- | --- | --- |
| `crates/infra-telemetry/src/logging.rs` | L,W,D,P: subscriber assembly, public logger owner and shared panic hook | Existing install function now returns `LoggerGuard`; hook no longer takes `PanicMessage`; private composition only | W/P | Current tracing/SDK/std; no process exit policy |
| `crates/infra-telemetry/src/logging/format.rs` (rename of `json.rs`) | L: one bounded capture path for both local formats | Private layer, bounded fields and visitors; replaces existing JSON file, no duplicate legacy path | L | Current serde/tracing, output sender; no sink or SDK diagnostic admission |
| `crates/infra-telemetry/src/logging/output.rs` (new) | W: actual new output resource lifetime | Public `LoggerGuard`/shutdown result via parent re-export; private queue/worker/counter state, no general writer trait | W | std channel/thread/io/time; no runtime requirement, telemetry provider or exit classification |
| `crates/infra-telemetry/src/logging/diagnostics.rs` (new) | D: one privacy rule must precede independent consumers | Private policy/visitor and crate-visible snapshot publication hook | D | tracing metadata/visitor and atomics; no custom OTLP parsing, raw-string storage or recursive tracing |
| `crates/infra-telemetry/src/traces.rs` | T: drain latch and truthful public outcome | Existing public handle, `ProviderShutdown::Completed/Incomplete`; private finite observation state | T | Current OTel exporter trait, metrics and runtime; no logger queue or exit policy |
| `crates/infra-telemetry/src/metrics.rs` | M: recorder/upkeep publish W/D observed state | Existing public Metrics surface; private publication state | M | Existing metrics recorder; no second pipeline or dynamic loss labels |
| `crates/infra-telemetry/src/lib.rs` | W,T,P: export current public boundary | Re-export guard/results/hook; remove obsolete PanicMessage export | W/T/P | Existing modules only; no behavior |
| `crates/infra-http/src/observe.rs` | H: first server observation source | Existing private span/access helpers; delete query denylist | H | Existing HTTP/tracing APIs; no generic redaction framework |
| `crates/infra-http/src/harden.rs` | P: existing HTTP recovery source | Existing panic response helper; sanitized category only | P | Existing HTTP/recovery APIs; no status/body delta |
| `crates/infra-grpc/src/observe.rs` | G: shared role-aware observer | Existing private make_span and tests | G | Existing gRPC/OTel helpers; no client-destination or cardinality redesign |
| `crates/service/src/bootstrap/mod.rs` | C: acquisition, failure reporting and outer runtime finish | Existing private bootstrap/error/run types, telemetry ownership slots | C | Existing infra-telemetry API; no duplicated formatter/queue |
| `crates/service/src/bootstrap/shutdown.rs` | C: shared deadline and vote | Existing private plan/budget/outcome types carry logger and tail marker | C | Existing lifecycle/infra API; no provider internals |
| `crates/jobs-worker/src/bootstrap.rs` | C: retain guards across failed preparation | Existing private resources/prepared/install/serve paths | C | Existing telemetry/dependency APIs; no business replay on telemetry loss |
| `crates/jobs-worker/src/shutdown.rs` | C: normal and abort cleanup | Existing private Plan/Resources/abort_startup; same ceilings | C | Existing telemetry API; no new grace knob |
| `crates/jobs-worker/src/lib.rs` | C: report primary failure before closure and preserve 0/1/2/3 table | Existing run/start/private result mapping | C | Existing bootstrap/telemetry; operator finite-command semantics unchanged |
| `crates/migrate/src/main.rs` | C,P: guard through terminal migration record | Existing entrypoint, private outcome/cleanup mapping and hook call | C | Current telemetry/runtime/migration APIs; no migration transaction/SQL change |
| `crates/infra-telemetry/src/traces/tests/delivery.rs` | T,D: real receiver and diagnostic boundary proof | Existing test module/fixtures | T/D | Existing HTTP/TLS fixtures; no production test-only API |
| `crates/infra-telemetry/tests/panic_hook.rs` | P,W: existing process-isolated shared-hook proof changes with payload-free API and returned guard | Keep its separate integration-test binary; remove PanicMessage argument and old payload-presence assertion, keep negative privacy/output proof and explicitly drain its logger | P/W | Existing public logger/hook APIs; no move inline or shared process-wide hook mutation |
| `crates/service/tests/lifecycle.rs` | C,H,P,W: built-process terminal and blocked-pipe proof | Existing process tests | C | Existing built binary/fixture controls; no alternate runner |
| `crates/jobs-worker/tests/process.rs` | C,P,W: worker built-process proof | Existing process tests | C | Existing retained-profile fixtures; no new environment |

Additional tests stay inline with their changed source owner, except the existing
process-isolated panic-hook binary explicitly retained above. If one existing proof
file cannot hold an implementation-local case, its sibling `tests` module is
permitted only under the same current owner and crate; this rule changes no
production ownership or test harness. No speculative exported testing seam.

Non-Rust consumers are the three existing documentation owners named in Design and
`scripts/ci/migration-validate.sh` only if its existing terminal-record assertions
need adapting. No manifest, lockfile or feature change is selected. The old
formatter path and old `Flushed`/`PanicMessage`
consumers have no retained compatibility branch. Current template profile markers
must keep each removable consumer and import coherent.
