# T2 — Bounded logging with complete consumer custody

Outcome:
Replace subscriber-emitter stdout I/O with the accepted bounded output writer, and migrate every shipped consumer so the last intended record and final exit are owned within existing deadlines. Shared API, diagnostics and consumers form one assembled buildable implementation boundary.

Consumes:
- [Specification: Logging progress and loss](../spec.md#logging-progress-and-loss-policy) and [logging lifetime](../spec.md#logging-lifetime-and-shutdown) — behavior, loss and exit precedence.
- [Mechanism: Complete records](../design/mechanism.md#complete-records-and-bounded-admission), [writer custody](../design/mechanism.md#writer-completion-and-custody), [evidence](../design/mechanism.md#loss-and-failure-evidence), and [terminal budgets](../design/mechanism.md#terminal-paths-and-existing-budgets) — closed API/mechanism and all consumer deadlines.
- [Ownership map](../design/ownership.md#files) and [libraries](../design/libraries.md) — placement, standard-library decision and resolved supported APIs.
- [Technical Design review](../technical-design-review.md) — discarded admitted backlog and the unconfirmed current record are included in stopped-writer loss.

Provides:
- One shared writer/guard/status/result API, metrics projection and all service/jobs-worker/migrate migrations, with corresponding tests and documentation. No consumer migration remains as later companion work.

Boundary:
Retain existing JSON/text formatting/filtering/correlation and use their MakeWriter seams. Add only the selected `logging/output.rs` module: standard 1024-record drop-newest channel, one stdout thread, admission-close linearization, explicit bounded loss/error state, truthful post-flush completion and nonblocking Drop. Include spawn/installation failure cleanup and the failure/panic close-and-account path without recursive or fallback output.

The same unit changes all three consumers. Establish the shared API before consumer lanes use it, then assemble their production and affected test callers. Service and worker retain an outer synchronous guard immediately after install, publish metrics status, preserve custody through startup failures/cancellation and terminal records, share the existing telemetry/grace deadline, and preserve the separate runtime shutdown allowance. Migration shares its existing one-second terminal allowance between runtime teardown and output drain while retaining its terminal-record order and committed-result semantics. Apply the specified degraded/failure exit mappings and primary-error precedence; overload drops alone do not change exit. The panic-hook in-memory fixture remains unchanged unless a mechanically required shared type assertion changes.

Logging/lifecycle documentation in this unit describes buffering, finite signals, terminal `logging.flush = "pending"`, final exit meaning and migration success-with-log-failure behavior. Correct inaccurate shutdown comments in the source owners as they are changed. T3 owns the separate business-work guidance. There is no new tracing-provider implementation, business/job policy, DB behavior, readiness dependency, CPU executor, manifest change, capacity configuration or performance claim.

Mutable owners:
- `infra-telemetry` output/logging public boundary (`src/logging/output.rs`, `src/logging.rs`, `src/lib.rs`), metrics projection (`src/metrics.rs`) and their existing test owners.
- `service` bootstrap/entry/shutdown (`src/bootstrap/mod.rs`, `src/bootstrap/shutdown.rs`) and associated process/test owners.
- `jobs-worker` bootstrap/entry/shutdown (`src/bootstrap.rs`, `src/lib.rs`, `src/shutdown.rs`) and associated process/test owners.
- `migrate` binary entry (`src/main.rs`) and associated binary/process test owners; no migration library or transaction edits.
- Logging/lifecycle sections in `docs/architecture/runtime-lifecycle.md`, `docs/configuration-source-policy.md` and migration operator contract in `docs/architecture/persistence.md`.

Exclusive locks:
- Shared telemetry writer API and its consumer signature migration; the T2 Lead establishes it before consumer lanes consume it.
- Runtime lifecycle and configuration-source documents shared with T3; T3 starts after this unit releases them.

Final validation:
- Claim: Enabled records do not wait for stdout/capacity, queue loss and writer failures are observable without that sink, and every shipped consumer reports actual completion or the specified incomplete outcome within its existing budget.
- Checks: Repository local completion for the assembled changed Rust/docs surfaces; retain existing record-semantics coverage and add material missing regression proof during implementation. Final assembled concurrency review covers admission/close/failure races, all consumer custody and budget/exit interactions. Concrete cases, fixtures, assertions and commands are Implementation-owned; no new benchmark campaign, infrastructure or mandatory DB experiment.
- Observable: Whole-record bounded admission and truthful loss/error/completion state at selected local boundaries, with compatible JSON/text records and the specified service/worker/migrate terminal outcomes. OS-writer completion is not collector durability; timeout does not claim an OS write stopped.

Reopen if:
Technical Design for unsupported selected APIs, missing lifecycle custody or contradictory shared deadlines; Specification only if loss, completion, compatibility or budget behavior must change. Mechanical caller/test repairs stay inside this assembled unit.
