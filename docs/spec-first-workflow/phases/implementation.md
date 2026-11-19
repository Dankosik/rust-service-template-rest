# Implementation

Use when accepted implementation work is ready and authorized. Implement the
planned tasks and their tests; validate the assembled result at the end.

## Carrier

In the session the user starts to execute a plan, open the named `tasks.md`
and check its current state under [Resume](../shared/resume.md). Report a
completed ledger as done and route an unready plan to its recorded owner.
For a ready ledger, bind this session as Ledger Orchestrator
(`LEDGER_ORCHESTRATOR`), including for a one-task ledger. Verify the selected
[Agent Harness](../../agent-harness.md) adapter's full-ledger controls before
dispatch. Report any capability gap that prevents full-ledger execution. The
[Planning Ledger Contract](planning/ledger-contract.md) governs scheduling,
canonical ledger writes, final acceptance, and cleanup.

### Acceptance-Unit Lead

An Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) owns its assigned unit or the
assigned final validation boundary. For implementation, use the unit packet and
return `Implemented` through [Acceptance Result V1](../interfaces/acceptance-result-v1.md)
as soon as the implementation is complete and its writers have joined. The
Orchestrator owns sibling scheduling and canonical ledger writes. A single
fixed inline unit uses a root-local Lead.

## Execution

Load the current task packet and consumed outputs. Read the touched production
code before its tests, then derive the task's tests from accepted behavior
contracts rather than mirroring internal steps or copying bloated neighboring
suites. Reuse sufficient tests, add only cases that catch realistic regressions
existing types and checks would miss, and consolidate overlapping or obsolete
tests around the strongest behavior check while preserving independent safety
and integration guards. Select minimal fixtures, independent assertions, and
the smallest proving layer locally under `rust-testing`—confirming when
practical that new or consolidated guards fail for the broken behavior—without a
preliminary matrix, approval, or separate test-planning task. Reopen only
genuinely unresolved product behavior or architecture; a missing, flaky, or
incorrect test stays with the executor.

For a non-trivial fixture, reuse an existing valid fixture or producer when it
stays minimal, or derive only the entities, relationships, and state transitions
the asserted contract requires, removing unused fixture setup you touch. Keep
that construction in test code, not a separate preparation artifact. Execution
follows the feedback and final-validation boundaries below.

Implement directly when handoff costs more than it saves. Run independent tasks
and useful subtask lanes in parallel when accepted interfaces are stable, writable owners and exclusive
locks are disjoint, and capacity permits. Integrate mutations serially. Apply
[Agent Harness](../../agent-harness.md) when delegating or replacing a lane.

Consumers may start from an agreed contract as soon as it is available. A
consumer that needs implemented code starts once that code is integrated into
the shared candidate. Writing tests, generating required sources, and reading
callers stay inside implementation.

### Feedback During Coding

Use bounded compile or type checks (`make lint-changed` or
`cargo check -p <crate> --all-targets --locked` per [Validation
budget](../../../AGENTS.md#validation-budget)), or one cheap local boundary
probe when an unverified assumption could cause substantial rework. Repair
observed in-scope defects before returning `Implemented`, and retain command
results for reuse under the Evidence Contract.

Establish shared contracts and generated sources before dependent lanes consume
them: when parallel tasks or lanes share a new or changed Rust seam, land the
boundary types, traits, and signatures first (verifying them with `cargo check`
and replacing any temporary placeholder bodies before returning `Implemented`)
so producers and consumers code against checked types rather than coordinating
in prose. Reopen the smallest decision owner only when implementation exposes
invalid accepted behavior or architecture; keep test-case, oracle, caller, error,
cleanup, locator, fixture, and runner repairs with the executor. When a write
surface grows, route the scope delta to the scheduling owner (Orchestrator, or a
root-local Lead directly) before editing outside the assigned scope, updating
locks and continuing the lane when disjoint without reopening Planning.

Join or stop child lanes, return [Delegated Result V1](#delegated-result-v1) to
the Acceptance-Unit Lead (or `Implemented` via [Acceptance Result
V1](../interfaces/acceptance-result-v1.md)) immediately, and keep the Lead
available for final-validation repairs. The scheduling owner integrates the
result before reassigning its writable scope.

## Final Validation

Once every planned implementation task is `Implemented`, the changes are
assembled, and all writers have joined, the delivery owner checks each material
behavior change against its accepted source. Trace the change through the
affected entry paths on the assembled candidate. For an
addition or modification, check the target behavior. For a removal, check that
the old behavior is unreachable on the paths from which it was withdrawn. For
a requirement rename alone, check that its promised behavior still holds.

Select the checks for the changed surface from the [Validation
budget](../../../AGENTS.md#validation-budget) and reconcile any task additions
with their authoritative source. Match each claim to current evidence at the
required boundary. One result can cover several claims. Consult [Validation
Routing](../../validation-routing.md) for mixed or specialized surfaces and the
[Evidence Contract](../shared/evidence-contract.md) for reuse, claim scope, or
unavailable infrastructure. Run fast checks first. Include variants, generated
clients, or intermediate versions when the selected checks or accepted scope
require them.

Apply the [review triggers](../shared/review.md#trigger) to the assembled
candidate to select full review, structure-only review, or direct completion.
The delivery owner validates behavior on every route. Keep the candidate fixed
during validation and any selected review. Join or stop readers before repair,
then run the failed, invalidated, and outstanding required checks.

- **Runtime or fixture scenarios**: When explicitly required, run the smallest
  end-to-end scenario covering setup, behavior, observation, and bounded cleanup
  first, reusing valid prepared environments.
- **Isolated or remote execution**: Resolve candidate or image digests,
  checksums, and host paths from task state or its manifest before consuming
  results, updating replacement locators and invalidating only affected proof
  after repair.
- **Long checks and resource waits**: Record an observation checkpoint in
  existing execution state and inspect logs, processes, or lock owners via
  background execution so concrete delay causes are visible in place.
- **Failure classification and repair**: Classify failures as product,
  test/oracle, or environment. Record material gaps when an optional check's
  environment is unavailable; repair required-check and in-scope product or test
  failures at their shared root inside the executor loop (releasing validation
  locks while editing) and apply [Parent-Owned
  Recovery](../shared/transition.md#parent-owned-recovery) when repeated
  attempts add no discriminating evidence.

Report the [Completion Result](../interfaces/acceptance-result-v1.md#completion-result)
for the requested scope, carrying forward the local validation evidence.

## Delegated Result V1

Every mutable delegated worker returns:

```text
result: <completed bounded outcome or no result>
changed_paths: <repository-relative paths or none>
proof: <omit for initial implementation; actual scoped results and gaps for final-validation repair or standalone verification>
candidate: <identity when isolated or none>
gap: <remaining blocker or none>
```

The result is input to the Acceptance-Unit Lead, not acceptance, review, or
ledger state.
