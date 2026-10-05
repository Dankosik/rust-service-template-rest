# Ownership Map V1

Status: ready

Authority: [mechanism](mechanism.md), [library decision](libraries.md), [specification](../spec.md).
All source is manual; no generated API contract or crate graph changes. Existing
bootstrap roots keep process policy. No new crate, trait, dependency, configuration
surface or alternate synchronous logging path is selected.

## Responsibilities

| Responsibility | Affected path / current evidence | Semantic owner / exact action | Boundary, cleanup, proof and reopen |
| --- | --- | --- | --- |
| Upload quantum | `PutBody::stream -> ExactLength::poll_frame`, `crates/infra-object-storage/src/body.rs` currently loops over ready empty frames | `infra-object-storage`: add local 64-poll quantum and self-wakeup | Existing private Body adapter, existing state/length authority. No spawned resource. Proof beside body and existing upload tests. Reopen if a quantum requires changing length/trailer semantics. |
| Output custody | `install_subscriber -> JsonLayer/fmt MakeWriter`, presently stdout | `infra-telemetry`: add `logging/output.rs` for bounded writer, guard, status/result; wire it in logging.rs and reexport required types in lib.rs | Standard-library rung, Rust 1.99 and subscriber 0.3.23; strongest rejected source tracing-appender 0.2.5 per libraries.md. Guard closes/detaches, thread owns actual I/O. Proof beside output plus existing formatting tests. Replace custom adapter if maintained API meets full parity. |
| Diagnostics projection | `Metrics::render`, existing recorder and diagnostics route | `infra-telemetry`: attach LoggingStatus to Metrics and publish finite signals at scrape | Same crate, metrics facade; no task/readiness dependency. State outlives worker, no separate cleanup. Proof owned by telemetry metrics tests. Reopen for unavailable absolute counter support or different diagnostics lifecycle. |
| Service terminal integration | bootstrap install/serve/run and shutdown::run | `service`: outer guard slot, stage deadline, terminal error ordering, drain result to exit code | Composition root owns 5s shared stage/grace and unchanged 1s runtime wait; helper does no process policy. Existing service process tests and local mapping proof. Reopen on a lost early-return custody path. |
| Worker terminal integration | bootstrap install_observability/prepare/serve, shutdown::run/abort_startup, lib::start/run | `jobs-worker`: same outer guard/deadline custody, startup cleanup deadline and final result | Worker owns config grace and exit policy, telemetry does not depend on it. No provider/job behavior changes. Existing worker process tests and local mapping proof. Reopen on a startup or signal path that bypasses finalizer. |
| Migration terminal integration | `crates/migrate/src/main.rs` installs subscriber before runtime; terminal record follows runtime shutdown | `migrate` binary: guard, shared existing 1s terminal deadline, success-to-exit-1 finalizer | No migration transaction/library change; committed result fields unchanged. Existing binary/process proof. Reopen if preserving terminal record requires changing DB contract. |
| Contributor/lifecycle guidance | runtime lifecycle, rust-tokio method, jobs cancellation, logging policy | docs owners and `.agents/skills/rust-tokio/SKILL.md`: precise execution lifetime, progress and logging rules | Follow Prompt Maintenance / Skill Authoring when editing canonical method; generators own carriers. Static docs/instruction consistency only, no model-effect claim. Reopen if a new workload needs new business admission or CPU policy. |

## Files

Paths below are repository-relative. Each added or materially changed Rust file
maps to one of the responsibilities above; declarations name surfaces, not bodies.

| Path | Responsibility / present reason | Declarations / visibility / call-path | Lifecycle/error ownership; allowed dependencies; forbidden responsibility |
| --- | --- | --- | --- |
| `crates/infra-object-storage/src/body.rs` | Upload quantum | Private quantum constant and existing Body impl | Existing adapter owns mismatch/errors; present dependencies only; no general scheduler. |
| `crates/infra-telemetry/src/logging/output.rs` (new) | Output custody | Private MakeWriter/channel/thread; public guard, shutdown result and read-only status types via reexport | Thread owns sink, guard owns close/wait/drop; std and existing tracing/metrics types; no grace constants, exit codes, global registry or new executor API. |
| `crates/infra-telemetry/src/logging.rs` | Output custody | `install_subscriber` returns guard; typed spawn error; layer writer wiring | Installation owns failure cleanup; existing filters/layers remain; no duplicated format or process policy. |
| `crates/infra-telemetry/src/lib.rs` | Output custody | Reexport guard/status/result needed by binary consumers; adjust example if present | Public library boundary only; no new runtime. |
| `crates/infra-telemetry/src/metrics.rs` | Diagnostics projection | Optional reader on Metrics, `with_logging`, snapshot publication at render | Existing metrics recorder owns exported signals; no tracing fallback or new monitor. |
| `crates/service/src/bootstrap/mod.rs` | Service terminal integration | Outer optional guard/deadline slot passed by mutable borrow into serve; terminal reporting and final exit | Sole service process owner; uses telemetry public API; no writer internals. |
| `crates/service/src/bootstrap/shutdown.rs` | Service terminal integration | Existing Plan/run conveys absolute shared telemetry deadline, comments corrected | Existing stage budget owner; no new stage or tail; no blocking writer wait on Tokio. |
| `crates/jobs-worker/src/bootstrap.rs` | Worker terminal integration | Pass guard/deadline slot through install/prepare/serve, attach metrics reader | Preserve subscriber custody before fallible next step; uses telemetry API; no emitter-level shutdown. |
| `crates/jobs-worker/src/shutdown.rs` | Worker terminal integration | Existing run/abort_startup return/carry stage deadline under grace | Worker staged cleanup owner; no output implementation or job policy. |
| `crates/jobs-worker/src/lib.rs` | Worker terminal integration | start/run retain guard, terminal failure record and finalizer before runtime shutdown | Sole worker process outcome owner; pre-install fallback only; no operator-command behavior change. |
| `crates/migrate/src/main.rs` | Migration terminal integration | Local guard and existing terminal deadline/finalizer | Existing migration binary owns exit mapping; no DB schema/transaction changes. |

Implementation chooses tests while coding: private writer tests belong beside
output.rs, quantum tests beside body.rs; black-box additions extend existing
service/worker/migrate test owners according to the behavior they observe. If
an existing test file must update an event assertion or subscriber result type,
that is mechanically owned by the same responsibility. No new source file
beyond output.rs is required; narrow placement refinements follow the owner
rather than starting a generic utility module.

Documentation edits belong to `docs/architecture/runtime-lifecycle.md`,
`docs/configuration-source-policy.md`, `docs/background-jobs.md`,
`.agents/skills/rust-tokio/SKILL.md`, and the migration operator section in
`docs/architecture/persistence.md` (its current terminal-record/exit contract). Do not copy these rules
into AGENTS.md or add a second contributor policy. Generated carriers, if any,
are refreshed with existing repository scripts and verified by check-instructions.
