# Buffer and resource bounds
status: ready
Execution owner: /root (LEDGER_ORCHESTRATOR); Planning handoff consumed and native collaboration controls available.
Completion: S1–S6 are implemented on the assembled candidate with the selected native mechanisms and removable profiles, required local validation and one independent delivery review pass, and the authorized commit/push and one separate PR are completed with applicable CI results recorded separately. No merge or deployment.
Global constraints: [Specification](spec.md), [selected design](design/selected-design.md), [ownership](design/ownership.md), and the current [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md) govern. All test execution and reviews occur at one assembled Completion boundary after all writers join. Executors choose cases and commands; nearest falsifiers are behavior, not a required infrastructure matrix. The root binds LEDGER_ORCHESTRATOR and alone writes this index after handoff; fresh Leads return Implemented and one assigned delivery owner owns validation/acceptance. Source baseline is 5927ffbba351af2f7fb8635316bbfa4ae5b31da6 in codex/buffer-resource-bounds-20261005.

## Tasks

- [x] T1: Download collection reserves only its unread tail while preserving EOF and checksum ownership.
  - Depends on: none.
  - Provides: S1 / R1 and associated storage lifetime guidance.
  - Packet: [T1](tasks/T1-unread-download.md)
  - Implemented: bounded diff against 5927ffb; download.rs SHA256 464661e8e2c8a6a4409c2ef0181fe1b285cbffbfbd2d93898eb41b5543e013dd, shared object-storage.md current SHA256 2c80f0ae96c9b37a1504fc4d3da285e9aa8f18afdb3b602b56c218164d9906a7 after related T2 guidance; verification pending assembled Completion; writers joined.
- [x] T2: Current nonstreaming storage responses and GET errors are bounded before SDK collection.
  - Depends on: none; T1 shares storage fixture/guide locks, so those writers are serial.
  - Provides: S2 / R2 and storage envelope guidance.
  - Packet: [T2](tasks/T2-storage-response.md)
  - Implemented: bounded diff; response_limit.rs SHA256 52db35f506f56d384f8a6debdcd80bd930d8e88d1e156212be8bde287174796c, lib.rs SHA256 0efeae28464a56c374a33ca07cbac34e4a3db1e9cbf9c1ce4bed6beccbdc0041, tests.rs SHA256 a5ede841dd683d9a21cce423f359f52725efedd4e20828aa61db51027b0d1b17, object-storage.md SHA256 2c80f0ae96c9b37a1504fc4d3da285e9aa8f18afdb3b602b56c218164d9906a7; verification pending assembled Completion; unavailable Cargo feedback retained for final compile diagnostics; writers joined.
- [x] T3: Job payload preparation retains bounded output with identical success bytes and error precedence.
  - Depends on: none.
  - Provides: S3 jobs / R3 and jobs guidance.
  - Packet: [T3](tasks/T3-job-preparation.md)
  - Implemented: bounded diff; enqueue.rs SHA256 da0adbf4113a0904cec90b886ac5e1fe407e4de72e6eb5b1150c825bb6fbe2ce, background-jobs.md SHA256 357d8decd5fb45424ecdc6110aa7b92b06a305f8bdd66b0bb2ff5f5e101f9c53; verification pending assembled Completion; writers joined.
- [x] T4: Event preparation retains bounded output with unchanged envelope and outbox encoding.
  - Depends on: none; consumes the closed S3 jobs contract, not changed jobs code.
  - Provides: S3 messaging / R4 and outbox preparation guidance.
  - Packet: [T4](tasks/T4-event-preparation.md)
  - Implemented: bounded diff; prepared.rs SHA256 dde32647b5b1e0c72ce28d0d71fd07fcea6d4bf6150c9f242b20aaca7e2c6d38, postgres-transactional-outbox.md SHA256 7a3a98b03e0f46c8764b3b1b5a4fd9d3bf71c545661af5a4f56144947ec8d1fe; verification pending assembled Completion; writers joined.
- [x] T5: Redis outstanding work remains finite across cancellation with normal supervisor recovery.
  - Depends on: none.
  - Provides: S4 / R5 and cache guidance.
  - Packet: [T5](tasks/T5-cache-admission.md)
  - Implemented: four-file scoped diff SHA256 330ce984fcbb85d1fadeaaf3c27d21aa9abfba863d2f758ffea6062913f9c63c, including authorized cache-decisions consistency correction; verification pending assembled Completion; writers joined and unused Cargo-feedback request cleared.
- [x] T6: Native publication admission and cancellation ownership are bounded in every retained messaging profile.
  - Depends on: none; consumes the closed prepared-event/outbox contracts, not T4 implementation. Native patch, adapter changes and source/profile custody are inseparable lanes of this unit.
  - Provides: S5 / R6–R8 and messaging reserve/lifetime guidance.
  - Packet: [T6](tasks/T6-publication-window.md)
  - Implemented: 130-file bounded candidate SHA256 8c479a5042f907a6a2b0a501bec85fae245a831d711f34563ce7a69f63b203ee; file identities verified against /tmp/buffer-bounds-t6-candidate.json; Cargo.lock SHA256 dd9502f6bdc0b585980c94b7c6ddb79ebc381bf4353aae1d9c63074b90634e7c; archive/source and locked metadata custody reported, runtime/profile/dependency checks pending assembled Completion; all writers joined.
- [x] T7: Adopters can apply accurate feature-owned byte, count and lifetime policy using existing HTTP/gRPC APIs.
  - Depends on: none; guidance consumes accepted contracts without requiring implemented runtime changes.
  - Provides: remaining S6 / R9 audit explanations and recipes.
  - Packet: [T7](tasks/T7-adopter-guidance.md)
  - Implemented: scoped HTTP/utility/gRPC guide diff SHA256 d9fb50adf83ab41f73c9ba1fa2e8b6b8945281f17e68828fbfc2695c35722e88; auth/cache-weight conditional guides unchanged as already accurate; verification pending assembled Completion; writers joined.

## Completion ownership and gates

Execution: /root/publication_lead (ACCEPTANCE_UNIT_LEAD) owns the single assembled Completion boundary; all unit writers joined. Consolidated local proof, independent integrated review, authorized publication and exact-head CI are active; no acceptance result exists yet.

Current repair: draft PR #248 at fd1960b passed cheap gates/projections; CI quality exposed T2 clippy and response-limit fixture failures. /root/storage_lead owns anchored response_limit.rs/tests.rs repair under the delivery owner; all invalidated Rust proof remains pending. Draft publication is intermediate, not acceptance.

The root records each returned Lead identity at dispatch, integrates serially and releases its scopes after Implemented; all tasks above are independently consumable outcomes. T1/T2 share a fixture/guide lock and execute serially at that boundary. T3/T4 deliberately retain separate optional-profile owners. T6 keeps its vendor/native, adapter and removal layers together because none independently establishes S5. T7 changes guidance for existing unchanged behavior; it is not a delayed documentation layer needed to complete another task.

After all seven tasks are Implemented and assembled, the root assigns one delivery owner under [Implementation](../../docs/spec-first-workflow/phases/implementation.md), records its native identity here, and freezes the candidate for the consolidated checks and [Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md#integrated-candidate). The owner's authority is [AGENTS validation budget](../../AGENTS.md#validation-budget), [Validation Routing](../../docs/validation-routing.md) and the [Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md). The owner consumes executor-selected commands from the packets, deduplicates them, chooses the mixed-surface route and retains scoped results across repairs. Required matching build/workspace tests, documentation and selected dependency/delivery checks belong here; no unit creates another execution or review gate. No infrastructure provisioning, full-repository claim, benchmark or additional matrix is introduced.

The same final boundary retains existing CI-owned dependency, optional-integration, image and initializer/profile gates. Local Accepted, exact-candidate CI, and PR publication are separate claims. Push/one PR is authorized after its applicable local prerequisites; load [External Effects](../../docs/spec-first-workflow/shared/external-effects.md) at that action. No merge/deploy/infrastructure authority is present. A missing optional environment is a limitation, not a code blocker; failed mandatory proof remains incomplete. Root records the delivery owner's Completion result without repeating validation or acceptance. No Completion result exists yet.
