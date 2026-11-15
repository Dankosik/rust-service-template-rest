# Planning Ledger Contract

Use only after Planning selects a persisted [Task Ledger
V1](../../interfaces/task-ledger-v1.md).

## Task And Acceptance Boundaries

Each ledger task owns one coherent repository outcome and its final acceptance
criteria, keeping layers of that outcome together while using subtask lanes for
parallel implementation and recording partial subresults inside the existing
task status. Separate tasks only for independently consumable outcomes or
distinct external gates.

`Implemented` and `Accepted` are distinct milestones: `Implemented` records that
planned code, tests, and cleanup are integrated with writers joined, while
Global Completion owns final validation and review of the assembled tree under
[AGENTS.md](../../../../AGENTS.md#validation-budget) and, when needed, the
[Evidence Contract](../../shared/evidence-contract.md).

## Ready Frontier

Attach each dependency to the earliest stage that genuinely consumes it—coding,
final acceptance, or an external effect—using [Task Packet
V1](../../interfaces/task-packet-v1.md) annotations so agreed contracts unlock
consumer coding immediately while live providers gate only required runtime or
external-effect steps.

Start a task once its required decisions, consumed contracts or `Implemented`
code, disjoint writable scopes, and implementation authority are present,
keeping acceptance and external-effect gates at final validation or release.

Dispatch ready independent work up to capacity when writers and exclusive locks
are disjoint, serializing overlapping mutations and refilling the frontier as
soon as each `Implemented` result integrates:

- **Pending later gates**: Complete local contract-backed coding, return
  `Implemented`, and leave later acceptance or provider gates in the ledger;
  return `Blocked` only when coding itself lacks a required input or authority.
- **Lead reuse**: Reassign a Lead to a related ready task once its prior delta
  is integrated and its lanes have joined, re-reading the new packet and keeping
  at most one active writable scope per Lead while reserving Orchestrator
  capacity for integration and recovery.
- **Scoped stops**: Pause only conflicting writers on a write overlap and only
  downstream consumers on an invalidated contract, updating locators and locks
  in place while unrelated tasks continue.

## Acceptance Transition

During orchestrated execution, the Orchestrator is the sole writer of canonical
ledger state, verifying isolated candidate identities as integration
bookkeeping when consuming [Acceptance Result
V1](../../interfaces/acceptance-result-v1.md).

On `Implemented`, integrate serially into the local development candidate,
record implementation completion, release locks, and refill the frontier while
preserving external-effect boundaries (push, deploy, auto-deploy branches).
Return semantic merge conflicts to the owning Lead against the assembled tree
and invalidate affected proof; on `Blocked`, retain partial code and the missing
input, reconcile writers, and route [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery).

Maintain one replaceable result per task (using Git for history and checkboxes
strictly for `Implemented` progress), keeping `status: ready` whenever coding,
final validation, or owner-held repair can advance and reserving `blocked` for
genuinely unavailable required inputs, capabilities, or authority.

Run one consolidated final validation stage across the ledger:

- **Entry gate**: Assign the delivery owner final validation only after every
  planned code task in the ledger is `Implemented` and assembled with zero
  active writers.
- **Pending external actions**: Run local final validation immediately even when
  deployment or release steps remain pending; execute authorized external steps
  afterward and hold Completion on them only when the request includes them.
- **Consolidated proof and review**: Execute the combined proof under the
  [Evidence Contract](../../shared/evidence-contract.md) and any final review
  selected by [Review](../../shared/review.md); pass local `Accepted` once local
  mandatory criteria and blocking findings close while reporting requested
  external outcomes separately.
- **Repairs and verdict**: Route defects back to their implementation Leads,
  rerun only invalidated checks, and have the Orchestrator record the resulting
  verdict directly.

Mark `done` once final `Accepted` closes Completion, then apply
[Cleanup](../../shared/cleanup.md).

## Stop Rule

Planning is ready when each outcome has its scope, consumed inputs, writable
owners, final observable outcomes, and dependency timing. Concrete tests are
executor-owned implementation choices, not readiness inputs. Name a future live
prerequisite as a final gate only when the accepted task explicitly requires
that execution or external effect. Empty additional-proof lists need no
expansion. Preserve actual user-owned acceptance and external-effect requirements.
