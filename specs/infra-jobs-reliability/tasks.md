# Goal

status: done

Completion: One separate PR contains the assembled T1 reliability correction,
its meaningful regression coverage, generated sources, profile containment and
operator/rollout documentation. The delivery owner has established the ordinary
local build/workspace-test/docs criterion, resolved the assembled independent
review, and obtained the selected existing database, migration, metadata,
outbox and profile evidence through their repository local/CI owners. Report
the actual PR and CI state separately from local acceptance; no merge,
deployment or live queue action is part of Completion.

Global constraints: [Intent](intent.md), [Specification](spec.md),
[ready Technical Design](technical-design-transition.md), and
[T1's execution boundaries](tasks/T1-bounded-recoverable-jobs.md).
The existing root `/root` becomes the sole ledger writer on Implementation
handoff and assigns one Acceptance-Unit Lead and one delivery owner; they may
be the same actor. All code is assembled before final validation or review.
Follow the [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md)
and [Implementation](../../docs/spec-first-workflow/phases/implementation.md).

## Tasks

- [x] T1: Jobs remain bounded and in durable custody through failure, with safe single-job recovery and honest process observation.
  - Depends on: none; accepted behavior, system/ownership design and rollout are ready. Existing local/CI tooling is consumed at source generation or final validation, not as a preliminary coding gate. Adopter fleet stop, live migration and recovery prerequisites belong only to excluded deployment/live effects.
  - Provides: One coherent corrected jobs-worker deliverable, infra-jobs adapter, regression coverage, schema/SQLx/profile closure and operator guidance.
  - Packet: [tasks/T1-bounded-recoverable-jobs.md](tasks/T1-bounded-recoverable-jobs.md)
  - Result: Implemented; reused foundation plus 13 corrective files, pre-main-integration diff SHA256 `106e1ea116d2cf747ef50db19fd7e6891049ce7c4d8ebb3e11ac2c341501f8a0`. Writers joined; rustfmt/diff check passed, tests and compile not executed. Root merged current main `546a381` without conflict; final validation follows on pinned Rust 1.99.0.

## Completion result

Accepted: local Completion at `86b388431685ec950d5a0df7ec93aa5f3abf5bbe`;
evidence: [validation.md](validation.md), [independent PASS](implementation-review.md).
Workspace lint/build/tests passed (817 passes, zero failures, one CI-owned Go
fixture ignored), as did the selected scoped local steps. The failed aggregate
remains failed; consolidated claim-matched receipts establish this local result.
Accepted: global Completion at `86b388431685ec950d5a0df7ec93aa5f3abf5bbe`.
PR #228 is open and ready for review. CI `37034066726` and CodeQL
`37034066787` completed successfully on that exact head; all selected image,
integration, SQLx, provider and profile gates passed. No merge, deployment or
live recovery result is claimed. Git will archive these receipts; canonical
jobs/async/outbox/configuration guides own the durable decisions after cleanup.
