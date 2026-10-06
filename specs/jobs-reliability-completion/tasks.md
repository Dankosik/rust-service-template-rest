# Jobs reliability completion

status: ready

Completion: R1–R5 in the [accepted specification](spec.md) are implemented and
proved on one assembled candidate: contained handler destruction and visible
supervisor failure; an executable reading-counter recipe; real crash/restore
and reconciliation; bounded recovery measurements; and an initialized service
that keeps its working feature, schema, data and customization across the exact
baseline-to-candidate upgrade. Record the immutable commit, selected exact-head
PR/CI results and independent assembled-candidate review. Missing required
runtime or CI evidence leaves that scope incomplete.

Global constraints: [Intent](intent.md), [system design](design/system.md),
[ownership](design/ownership.md) and the [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md)
govern this ledger. Runtime baseline is
`ac88395be87cba3a1e0587f533dc50a71e358c8d`; Planning input HEAD is `c8cf7a0`.
Root binds `LEDGER_ORCHESTRATOR` for Implementation and becomes the sole ledger
writer. Leads return Implemented after code/test writing and joined writers;
checkboxes do not imply checks passed. One delivery owner validates only after
both tasks are assembled. No per-task tests/reviews as acceptance gates, new
environment solely for local completion, full-build/profile/harness matrix
multiplication, production effect, merge or purchase. Authorized publication
stays on `codex/jobs-reliability-followup-20261005`, draft PR #240.

## Tasks

- [x] T1: Every admitted attempt retains bounded, observable lifecycle custody.
  - Depends on: none; accepted R1 and R4 ownership-observation design is ready.
  - Provides: safe Pending-handler destruction, supervised retirement through the existing failure latch, and the two actual-owner gauges.
  - Packet: [T1](tasks/T1-attempt-custody.md).
  - Result: Implemented: T1; verification: pending final validation; candidate: `8d1dcadf1b9a3844f1433c21b1e95162fbedb8d7`, six-file diff SHA256 `ff14f71dad614444c9a7024fb98cd748cf50ca92f48f528dcfb9bbf2bffde6ab`. Coding diagnostic `cargo check -p infra-jobs --all-targets --locked` passed; behavioral and process proof remains pending.
- [x] T2: The executable reading-counter reference demonstrates durable effects and recovery across an actual template upgrade.
  - Depends on: T1 Implemented only for the final measurement binding and exact runtime-patch adoption wiring; all other implementation may start from the accepted design. T1 passing proof is not a coding prerequisite. Both tasks gate final acceptance.
  - Provides: fixture CLI/business/receiver/schema, combined real-process rehearsal and measurements, initialized-service adoption driver, source-only carrier integration and truthful usage guidance.
  - Packet: [T2](tasks/T2-executable-reference.md).
  - Result: Implemented: T2; verification: pending final validation; candidate: `47641cfb748e0dfae4674a3abd5ad52a1ce0cbac`, 21-file bounded tree SHA256 `27170197c461a65e151501581faf5cf311746535fa7294eeaad7e2492d2a17a1`. Static syntax/profile diagnostics passed. Integration Cargo check failed with ENOSPC in dependencies before fixture diagnostics; matching build, behavioral/runtime and CI evidence remains pending.

## Completion boundary

Delivery execution: `/root/jobs_delivery_resume`, fresh delivery Lead after
interruption made the earlier native owner unavailable; the assembled source
and pending proof are preserved. One validation/review/publication boundary. Local
Data-volume capacity was below 1 GiB at assembly; no caches or unrelated
resources were deleted. The delivery owner must retain this environment
limitation and obtain actual matching build/tests and required real evidence
through the existing authorized carrier.

Execution chooses concrete cases, assertions and commands while coding and
leaves their locators in the packets for the delivery owner. Reuse adequate
existing proof; final validation consolidates overlapping coverage:

- Editing feedback is bounded type-checking. Once assembled, perform the
  matching build and relevant tests; multiple crates/manifests select the
  workspace route under [Validation budget](../../AGENTS.md#validation-budget).
- Required real PostgreSQL/NATS, reference and initialized-service evidence
  follows the existing [validation owner](../../docs/validation-routing.md)
  and [Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md).
  Heavy gates are normally CI-owned; the accepted bounded local/CI diagnostic
  permits its concrete run on the existing carrier. Reuse binaries and results
  across the bounded scenarios, without repeating the full exercise for each
  profile or harness. Preserve existing selected CI gates.
- Stabilize and commit the executable candidate before R5 consumes its exact
  immutable SHA. Receipt-only documentation may follow; retain the original
  proof identity and confirm unchanged semantics. Never relabel earlier CI
  as exact-head evidence for a later commit. A source repair invalidates only
  affected proof, including candidate adoption when its runtime delta changes.
- Run static documentation checks and changed-carrier checks selected by their
  owners, then the required independent review of the assembled result.
  Publish/update the authorized PR and obtain actual selected exact-head CI
  results. Preserve failed-run diagnosis and dispose only recorded rehearsal
  resources after retaining evidence. No merge is part of Completion.

## Obligation reconciliation and readiness

T1 owns R1 and R4's actual ownership observation. T2 owns R2–R5's executable
reference, measurements and upgrade, all operational/example documentation,
and the source-confirmed stale HeaderValue prose correction from the
specification's proof boundary. No accepted obligation is deferred or removed.
There are two independent outcomes: the runtime fix is consumable without the
reference; the reference is one operational recipe whose CLI, receiver, schema,
driver, carrier and guides are layers, not separately acceptable fragments.

Written readiness walk: T1 implements inside the existing attempt/claim/engine
owners; T2 writes the fixture and driver against the closed public contracts
concurrently. Their Rust files and manifests do not overlap. T2 owns shared
guides and carrier files. When T1 lands, T2 binds the actual gauges and selected
source patch; neither waits for a per-task review or heavy run. After both
writers join, the delivery owner fixes the candidate and runs consolidated
validation, rehearsal and final review. No user-owned decision is outstanding.
Review status is recorded in [Planning Review](planning-review.md).
