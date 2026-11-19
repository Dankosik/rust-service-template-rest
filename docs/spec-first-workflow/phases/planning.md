# Planning

Use when structured work lacks one ready fixed implementation unit or the
smallest dependency-ordered ledger. Own order, unit boundaries, dependencies,
and final success conditions; never invent product behavior or architecture.
Leave test cases, fixtures, assertions, proving layers, and exact test commands
to Implementation. Do not require their prior design or approval.

Consume ready behavior, design, rollout, risk, and repository-owner inputs.
For each accepted obligation, identify the task deltas, final validation, or
external action that will satisfy it, current evidence that it is already met,
or its authorized scope exit. Trace each task to the obligation it serves,
directly or through a prerequisite.
Compare its current-to-target change and acceptance criteria with the source,
including limits and exceptions. Anchor the accepted mechanism and its owner.

Before expanding task packets, establish each Outcome, dependency, and
final observable, then apply [Task Review / Readiness's atomicity
criterion](task-review-readiness.md#atomicity-gate) as author self-check. Correct
unit boundaries in the same plan before detailing their packets; this adds no
intermediate artifact or review gate. The completed plan still receives the
independent review below.

For the [implementation handoff](../shared/transition.md#implementation-handoff),
persist [Task Ledger V1](../interfaces/task-ledger-v1.md) even for one executable
unit. If no implementation delta remains, submit the supporting sources in the
phase result for independent review without creating a ledger. Name the owner
and stage of any outstanding proof or external effect. If a missing required
input prevents a conclusion, record the blocker and its owner. A fixed unit may
stay inline only when no durable boundary exists.

Keep dependencies, status, and results in `tasks.md` and each [Task Packet
V1](../interfaces/task-packet-v1.md) in `tasks/<ID>-<slug>.md`. Packets cover
implementation and test writing. [Feedback During Coding](implementation.md#feedback-during-coding)
governs checks during implementation. Completion owns required validation and
any selected review of the assembled candidate.
Apply the [Planning Ledger Contract](planning/ledger-contract.md). Load
[Conditional Planning Branches](planning/conditional-branches.md) only for
integration-first uncertainty or broad contract fan-out.

## Readiness Dry Run

Check the whole plan against Completion. Could every task meet its criteria
while an accepted outcome still fails? Trace each dependency to the source,
producer, or external owner that can supply it by the stage that consumes it.
The order must allow all tasks to reach `Implemented` before final validation.
For example, coding cannot depend on the assembled plan's `Accepted` result.
Keep an explicitly required release approval at the release step.

Use the [Ready Frontier](planning/ledger-contract.md#ready-frontier) to identify
every task eligible to start immediately, regardless of listing order or worker
capacity. Walk each through implementation from the handoff alone. Confirm
the existence and semantics of each existing API, type, configuration key, or
other input it consumes against the current source. For a crate, use the
source of the version locked in `Cargo.lock`. Reuse source checks shared by
several tasks. If a task marked ready lacks a required input or its source
contradicts the planned use, correct its dependencies or reopen the input.

For later tasks, check their promised inputs, outputs, and dependency timing.
Leave dispatch order to the Orchestrator as the frontier changes. This is a
written walkthrough without tests, services, or probes.

Submit the fixed plan or no-implementation result to independent
[Task Review / Readiness](task-review-readiness.md) under shared
[Review](../shared/review.md). Reopen the smallest owner of any missing
decision or required input.
