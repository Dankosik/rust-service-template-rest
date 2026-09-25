# Ownership Map V1

Status: ready. [System design](system.md) owns mechanism; unchanged crate edges
and composition roots remain. No new crate or provider/feature dependency.

## Responsibilities

| Responsibility | Affected path/current evidence | Semantic owner and exact action | Boundary and cleanup | Proof owner / reopen |
| --- | --- | --- | --- | --- |
| Queue storage/admission | Immutable bytea migration; enqueue serializes before insert; startup checks columns | `migrations/20260925000001_simplify_background_jobs.sql` owns schema; `enqueue.rs` owns validation, UTF8 producer guard and text binding; `maintenance.rs` owns worker admission | Forward-only conversion; add to profile removal inventory. No global pool policy or SQLx feature change. | integration-tests jobs enqueue/migration coverage; reopen System Design if atomic compatible conversion is impossible. |
| Handler policy/API | `kind.rs` owns Job, JobError, JobKind, dispatch | Extend that owner for private generation, completion method, checked retry/snooze, const names; `enqueue.rs` owns shared delay-domain conversion; `lib.rs` exports supported API | No feature imports jobs; completion delegates to crate-private outcome SQL. No generic transaction wrapper or local committed flag. | Kind/enqueue unit owner and PostgreSQL transactional jobs coverage; reopen for a changed public behavior. |
| Claim authority | `claim.rs` policy arrays, send/decode/dispatch, current locking CTE | Retain file; replace claim query and admission, attach static deadline/random draw | Uses existing sequence, pool and transaction owner. Remove unknown-claim collections/readback. | Existing jobs execution PostgreSQL owner; reopen if final-only locking/index plan cannot preserve disjointness. |
| Execution and persistence | `attempt.rs` handler JoinHandle, intended outcome/write/reconcile | Retain file; supervisor owns lifecycle/result and shared COMPLETE execution callable by Job method | Delete attribution functions, splitmix64, readback; all outcomes stay fenced. Rust/application policy over existing PostgreSQL random and in_tx is the selected reuse rung; new RNG or queue framework rejected as unnecessary. | Unit timing/error tests and jobs execution transaction proof; reopen only for a new required disposition or inability to bound cleanup. |
| Engine lifecycle | `engine.rs` Attempts map, TaskTracker, controls; `lease.rs` extends/releases/reconciles | Engine keeps process controls, tracker, budgets and aggregate local counters. Move release SQL into attempt.rs beside other outcomes; delete lease.rs entirely | No replacement per-attempt registry. Supervisor owns terminal decision; root only signals/waits. | Existing engine/worker process owners; reopen for required detached execution. |
| Observation/retention | `maintenance.rs` SAMPLE/aggregate/publish and retention | Replace sampler with capped per-kind indexed sampling and freshness; retain existing bounded retention statements | Remove unknown aggregate and unbounded live GROUP BY; no observer election. | Unit publication semantics plus PostgreSQL query plan/queue observations; reopen on measured per-worker cost. |
| Trace propagation | `traceparent.rs` custom codec; telemetry installs propagator | Rename to `trace_context.rs`; use installed API with bounded two-field carrier | No baggage/global installation/new telemetry dependency; remove old parser/formatter. | Adjacent module tests with propagator/tracing link context; reopen only for new propagation contract. |
| Worker integration | `jobs-worker/src/shutdown.rs` invokes release and logs durable attribution | Update cleanup API/observed counters and startup-error display where consumed; retain policy/stage order | No shared lifecycle crate now; root evidence found duplicated signals/deadline/join but distinct ordering/budgets. | Existing worker unit/process proofs; reopen extraction on a real shared primitive change. |
| Adopter/profile/future reuse | Background-jobs guide, architecture leaves, migration README, roadmap; template_profiles.json removes one migration | Replace contradicted active contracts, add new migration removal, record 10.5/10.6 reuse and fairness/extraction limits | Documentation/profile inventory only; no new webhook/outbox product or CI gate | Static consistency/docs-check and existing CI initializer matrix; reopen for a changed supported profile. |
| Test consolidation | `test/tests/jobs/enqueue.rs` e6 holder/isolation histories | integration-tests retains every distinct blocking/Created/Duplicate/40001/usability oracle; implementation decides table/setup consolidation under test-audit | No new production seam for test choreography; deleted obsolete internal registry/codec tests leave with their mechanisms | Existing integration-tests owner; reopen only if an oracle cannot be retained. |

## Files

Files below have exact current responsibilities; new test filenames are not an
architecture decision. Add cases to the existing owning suite/module unless a
new independently cohesive migration fixture requires a file beneath that suite.

| Rust path | Responsibilities / present reason | Declarations/visibility and call role | Lifecycle/errors and allowed dependencies | Forbidden responsibility |
| --- | --- | --- | --- | --- |
| `crates/infra-jobs/src/enqueue.rs` | Queue storage/admission; handler delay domain | Existing public enqueue/options/errors plus crate-private checked delay conversion | Caller connection/transaction; existing serde_json/sqlx/kind | Commit, pool acquisition or retry loop |
| `crates/infra-jobs/src/kind.rs` | Handler policy/API | Existing public kinds/job/handler; private generation; public completion error/method, delay error and const assertion | Adapter-facing; existing dependencies plus sibling outcome execution | Process lifecycle; arbitrary business replay |
| `crates/infra-jobs/src/claim.rs` | Claim authority | Crate-private claim SQL/loop/decoded rows | Engine tracker/slots; existing sqlx/in_tx/kind | Outcome attribution, heartbeat |
| `crates/infra-jobs/src/attempt.rs` | Execution and persistence; release from engine lifecycle | Crate-private supervisor/intent/outcome SQL; narrow completion executor for kind method | Owns handler join, cancellation and persistence deadline; existing Tokio/SQL/metrics | Process signal install or second caller transaction |
| `crates/infra-jobs/src/engine.rs` | Engine lifecycle | Public Engine/Started/DrainEnd/startup and operation errors; private shared process controls | Owns tracker/start/stop/forced cleanup; existing dependencies | Per-attempt result/deadline registry or business policy |
| `crates/infra-jobs/src/lease.rs` | Engine lifecycle cleanup | Delete file and module declarations | Upkeep/reconcile removed; release joins outcome owner | No compatibility alias |
| `crates/infra-jobs/src/maintenance.rs` | Observation/retention; startup admission | Existing public constants and crate-private queries/publisher | Engine operation budgets; existing metrics/sqlx | Global queue inventory, observer leadership |
| `crates/infra-jobs/src/trace_context.rs` (renamed) | Trace propagation | Crate-private carrier/capture/link | Existing opentelemetry/tracing bridge; invalid becomes absent | Handwritten W3C codec, baggage |
| `crates/infra-jobs/src/lib.rs` | Supported API exports | Export new completion/delay/const helper; replace obsolete lease/codec exports and docs | No runtime owner or new edge | Duplicate implementation |
| `crates/jobs-worker/src/shutdown.rs` | Worker integration | Existing process-local plan; consume renamed cleanup result | Keeps signals, stage ceilings, degraded vote, pool/telemetry shutdown | Queue SQL/result arbitration |
| `crates/jobs-worker/src/bootstrap.rs` | Worker integration | Only if exhaustive startup error handling needs the UTF8/schema refusal | Existing composition; no added service-side jobs activation | Engine internals |
| `test/src/jobs.rs`, `test/tests/jobs/{enqueue,execution,process,http_idempotency}.rs` | Test consolidation and changed public/storage contracts | Existing test-only adapters/fixtures; update only actually affected assumptions | Existing admitted real-DB harness; errors/transactions reach production seams | Shipped runtime helper or implementation mirror |

Static self-review: each runtime responsibility stays with its existing crate;
only the codec filename changes and lease.rs disappears. No surviving
cross-crate placement fork or new boundary requires a complementary ownership
panel. Fresh Technical Design Review still checks the fixed complete design.
