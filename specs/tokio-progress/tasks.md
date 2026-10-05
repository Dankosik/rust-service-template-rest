# Tokio progress delivery

status: ready

Completion: The assembled candidate satisfies [the accepted outcome](spec.md#composition-and-completion): bounded upload polling with exact-length compatibility; bounded best-effort logging with observable loss/failure and deadline-owned completion in every production consumer; and current business-work guidance. Implementation and its tests are complete, the repository's applicable consolidated validation and final assembled concurrency review pass with no unresolved in-scope defect. The root publishes the authorized separate PR and reports its exact-head CI result; local acceptance alone does not establish publication or CI success. No merge or deployment.

Global constraints: [Intent](intent.md), [Specification](spec.md), [mechanism](design/mechanism.md), [ownership](design/ownership.md), and [library decision](design/libraries.md) remain authoritative. Execute under [Implementation](../../docs/spec-first-workflow/phases/implementation.md) and the [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md). Concrete tests and commands belong to Implementation. All task proof and the required concurrency review belong to one final assembled validation boundary. No dependency, runtime knob, pool, host thread tuning, infrastructure or benchmark campaign is selected.

## Tasks

- [x] T1: Upload wrapper returns control within its 64-inner-poll quantum without changing exact-length behavior.
  - Depends on: none.
  - Provides: Cooperatively yielding upload adapter with its implementation-owned regression coverage.
  - Packet: [T1 upload progress](tasks/T1-upload-progress.md).
  - Implemented: verification pending final validation; `body.rs` blob `c33e2211709f57fda1e4248f4fba3655d300c1f7` on base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
- [x] T2: Every shipped logging consumer uses the bounded output writer and owns truthful, deadline-bounded final completion.
  - Depends on: none.
  - Provides: Buildable shared writer, metrics projection, service/worker/migrate integration and corresponding logging/lifecycle documentation and coverage.
  - Packet: [T2 logging completion](tasks/T2-logging-completion.md).
  - Implemented: verification pending final validation; bounded 17-file candidate SHA-256 `638de497774d97978dbf7bbcf6be3088eed1fd9233ed5f46affd26cae9a15a47` on base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
- [x] T3: Runtime and contributor guidance accurately owns blocking/CPU execution beyond waiter cancellation.
  - Depends on: T2 Implemented and assembled, solely to release shared runtime/configuration documentation before editing it; no validation or acceptance gate.
  - Provides: Canonical business-work rules, corrected stale runtime descriptions, job guidance linkage and synchronized instruction carriers where required.
  - Packet: [T3 business-work guidance](tasks/T3-business-work-guidance.md).
  - Implemented: verification pending final validation; canonical runtime/config/job guidance and rust-tokio skill, with existing generated views refreshed, on base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

## Boundary and scheduling decision

Final delivery owner: `/root/delivery`; all implementation writers have stopped. Consolidated execution proof and required independent concurrency review are pending. The root may publish an explicitly unaccepted draft while validation is pending, as CONTRIBUTING permits; ready promotion remains gated by actual proof and final review. Exact-candidate CI execution may supply proof for its CI environment and is never reported as a workstation pass.

These are three independently consumable outcomes: upload progress can ship without logging changes; the complete logging migration can ship without the upload adapter; contributor admission/lifetime guidance can govern later features independently. They justify this compact ledger rather than a synthetic single unit containing unrelated postconditions. The writer API, diagnostics and all three consumer migrations are inseparable layers of T2 and therefore do not become separate ledger tasks. T2 establishes its shared API before dependent consumer lanes consume it, and returns only the assembled buildable change.

T1 and T2 form the initial ready frontier. T3 has no unresolved semantic input, but shares the runtime/configuration document owners with T2 and waits only for that write scope to be released. There are no persisted execution waves or per-task check/review gates. The root binds as `LEDGER_ORCHESTRATOR`, owns canonical ledger updates, dispatches fresh Acceptance-Unit Leads through the Codex harness and assigns one final delivery owner after all writers finish. Publication and exact-head CI are later external-effect/reporting boundaries under the root's existing authority, not prerequisites for coding.
