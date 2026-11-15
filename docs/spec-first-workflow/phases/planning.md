# Planning

Use when structured work lacks one ready fixed implementation unit or the
smallest dependency-ordered ledger. Own order, unit boundaries, dependencies,
and final success conditions; never invent product behavior or architecture.
Leave test cases, fixtures, assertions, proving layers, and exact test commands
to Implementation. Do not require their prior design or approval.

Consume ready behavior, design, rollout, risk, and repository-owner inputs.
Reconcile every accepted implementation-changing obligation
to one independently acceptable task, named task deltas, proved
no-implementation, or scope exit.

Delta: Make the current-to-target change explicit and anchor its accepted
mechanism and ownership. Leave routine coding choices to Implementation.

Before expanding task packets, establish each Outcome, dependency, and
final observable, then apply [Task Review / Readiness's atomicity
criterion](task-review-readiness.md#atomicity-gate) as author self-check. Correct
unit boundaries in the same plan before detailing their packets; this adds no
intermediate artifact or review gate. The completed plan still receives the
independent review below.

Return one fixed inline unit when no durable boundary exists. Otherwise persist
[Task Ledger V1](../interfaces/task-ledger-v1.md), keep dependency, status, and
results in `tasks.md`, and put each task packet in `tasks/<ID>-<slug>.md` using
[Task Packet V1](../interfaces/task-packet-v1.md). Packets contain code and test-writing work.
Test execution and reviews belong
to Completion, not schedulable ledger tasks. The packet defines what must become
true; Implementation chooses how. Apply the [Planning Ledger
Contract](planning/ledger-contract.md). Load [Conditional Planning
Branches](planning/conditional-branches.md) only for integration-first
uncertainty or broad contract fan-out, and complete the [Readiness dry
run](#readiness-dry-run) below before declaring readiness.

## Readiness Dry Run

Walk the next task through its implementation using accepted behavior, design,
current sources, and dependency timing under the [Ready
Frontier](planning/ledger-contract.md#ready-frontier). This is a written
walkthrough; do not launch tests, services, or probes.

Confirm each existing item the task consumes — such as a function, type, trait
implementation, configuration key, or library API and its semantics — against
its source: current repository code, and for a crate, the source of the version
locked in `Cargo.lock`. A plan's or design's description of that item is not
evidence. A consumed item that is missing, or whose source contradicts the
planned use, leaves the task unready.

Preserve real external dependencies at their consuming action; an unavailable
future test environment does not block coding from closed inputs. Do not persist
waves; the Orchestrator refills the ready frontier after implementation results,
and Leads choose useful parallel subtask lanes.

Apply [Task Review / Readiness](task-review-readiness.md) through shared
[Review](../shared/review.md). Ready when the next unit can implement from
closed inputs without chat history or a new decision, with proof placed in
final validation and later dependencies annotated under the [Ready
Frontier](planning/ledger-contract.md#ready-frontier). Reopen the smallest
upstream owner of an input missing for the next authorized action.
