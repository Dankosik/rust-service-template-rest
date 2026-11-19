# Unit Implementation

Use as Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) for one assigned unit, or
as a lane working a subset of it. The Lead turns one task packet into code and
tests that meet the [Implemented
bar](../../interfaces/acceptance-result-v1.md#implementation-result). The
Orchestrator owns sibling scheduling and ledger writes. [Final
Validation](final-validation.md) owns proof of the assembled candidate.

## Implement

Read the task packet's `Outcome`, `Depends on`, `Sources`, and `Boundary`. Locate the code
that owns the behavior and the entry point or downstream consumer that must
observe the change. Implement the outcome along that path, including the
wiring and cleanup the packet requires. Writing tests, generating required
sources, and reading callers are part of the unit.

## Tests

Read the affected production code before its tests. Derive expectations from
the accepted behavior and reuse tests that already protect it. Add coverage
for realistic regressions the existing checks would miss. Preserve independent
safety and integration guards when consolidating overlapping cases. The Lead
chooses the proving layer and cases while coding, fills coverage gaps, and
fixes flaky or incorrect tests. For Rust, use
[Rust Testing](../../../../.agents/skills/rust-testing/SKILL.md) to select
assertions and fixtures and check whether a test detects the intended regression.

For a non-trivial fixture, reuse an existing valid fixture or producer when it
stays minimal. Otherwise construct only the state the assertion needs. Keep
fixture setup in test code and remove unused setup you touch.

## Lanes

Implement directly when handoff costs more than it saves. Run useful lanes in
parallel when accepted interfaces are stable, their writable owners and
exclusive locks are disjoint, and capacity permits. Integrate their mutations
serially. Apply [Agent Harness](../../../agent-harness.md) when delegating or
replacing a lane.

Establish shared contracts and generated sources before dependent lanes consume
them. When lanes or parallel units share a new or changed Rust seam, land its
boundary types, traits, and signatures first and verify them with `cargo
check`, so producers and consumers code against checked types rather than
coordinating in prose. A consumer of an agreed contract may start as soon as
the contract is available. A consumer of implemented code starts once that
code is integrated into the candidate.

## Checks While Coding

Type-check as you edit with `make lint-changed` or `cargo check -p <crate>
--all-targets --locked` per [Validation
budget](../../../../AGENTS.md#validation-budget). Add one cheap local boundary
probe when an unverified assumption could cause substantial rework. A single
focused test can serve as that probe. Repair observed in-scope defects, and
rerun the probe that exposed one while it stays cheap and available. Retain
results for reuse under the [Evidence
Contract](../../shared/evidence-contract.md#execution-evidence).

## When The Packet Does Not Fit

| Situation | Action |
| --- | --- |
| A test case, oracle, caller, error path, cleanup, locator, fixture, or runner needs repair | Repair it inside the unit. |
| The change needs a path outside the assigned writable scope | Send the scope delta to the Orchestrator before editing there; a root-local Lead decides it directly. Continue once the added scope is disjoint and its locks are updated. Planning stays closed. |
| Implementation exposes invalid accepted behavior or architecture | Finish the work that input leaves unaffected, then return `Blocked` naming the decision and its smallest owner. A root-local Lead reopens that owner under [Transition](../../shared/transition.md). |

A lane raises the last two situations with its Lead through `gap`.

## Return

Join or stop every lane. Return the [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result) as soon
as the unit meets the Implemented bar, recording in `decisions` and
`unverified` what the Delivery Owner cannot read from the diff. Stay available
for final-validation repairs. The Orchestrator integrates the result before
reassigning the writable scope.

A lane returns Delegated Result V1 to its Lead.

## Delegated Result V1

Every mutable delegated worker returns:

```text
result: <completed bounded outcome or no result>
changed_paths: <repository-relative paths or none>
proof: <checks actually run with their results, or none>
candidate: <identity when isolated or none>
gap: <remaining blocker or none>
```

The result is input to the Acceptance-Unit Lead, not acceptance, review, or
ledger state.
