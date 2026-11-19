# Task Review / Readiness

Use when shared [Review](../shared/review.md) routes a fixed Planning result
or the user requests standalone readiness review. For an inline unit or
ledger, apply the checks below. For a no-implementation result, go directly
to its [verdict](#verdict).

## Atomicity Gate

Before simulating execution, test whether each packet is one independently
acceptable repository outcome. Return `FAIL` to Planning and name the
boundaries when the packet contains more than one Outcome, or when it is only a
layer of another Outcome and cannot be consumed without still-planned companion
work. Independent implementation, investigation, or repair inside one
postcondition is not a split. Do not author the replacement ledger.

## Lenses

Compare the whole plan with the accepted sources in both directions. Look for
an omitted obligation, a packet that changes a cited rule, or a task whose
removal would leave all obligations and prerequisites satisfied. Trace a path
to Completion through the recorded dependencies at their consuming stages.
An unknown producer, a cycle, or a result needed before it can exist breaks
that path.

Independently reconstruct the [Ready Frontier](planning/ledger-contract.md#ready-frontier)
and apply the [Readiness Dry Run](planning.md#readiness-dry-run) to each task
in it individually. Trace each expected outcome through its real entry point,
including canonical and generated sources, writable owners and exclusive locks. Inspect
deferred work in detail only when a dependency or contract is in doubt.

Trace persisted artifact custody and status through each actor boundary; the
next actor must proceed from canonical state without chat reconstruction.

Falsify each changed contract/authority against current producers, consumers,
derived outputs, mirrors, proof carriers, and replacement surfaces. A companion
must be inside the unit, behind a dependency whose intermediate state remains
valid, or proved unchanged. Stop a blocked path at its earliest unrecorded
choice or unavailable input and continue checking independent paths.

Do not reslice tasks, author replacement packets, or choose missing behavior,
mechanism, ownership, rollout, authority, or concurrency.

## Verdict

For an implementation handoff, `PASS` requires a plan that covers the accepted
outcome, justifies its tasks, and has a feasible path to Completion. At least
one task can start. The plan's frontier must match the tasks the reviewer
could start individually from verified inputs, without chat history or a
new product or design decision. Specific test cases and commands belong to
Implementation. A required external gate applies at its consuming action.
Missing optional integration evidence does not block readiness.

For a no-implementation result, check the grounds directly in the phase
result. `PASS` requires current evidence that each implementation obligation
is already satisfied or a scope exit authorized by its decision owner.
Separately requested proof or external effects remain outstanding until
their owners supply the result.

`CONCERNS` may carry a bounded later risk consistent with readiness.
Return `FAIL` for an unmet readiness criterion, an invalid split, or a hidden
decision. Name the smallest owner that can repair the plan or supply the
missing decision or input.
