# Review

Use fresh independent review at the boundaries selected by this contract.

## Trigger

Run one fresh independent review for a standalone Research synthesis and for
each completed Specification, Technical Design, and Planning result before phase
movement. Keep implementation tasks, subtask lanes, and test authoring moving
directly along the ready frontier, reserving implementation review strictly for
the final assembled delivery boundary (or an explicit standalone review
request).

At the final assembled delivery boundary (in both structured workflows and
standalone Direct Work), select the disposition for the changed surface. Assign
one fresh reviewer when full or structure-only review applies.

- **Full final review**: Required for any multi-task ledger (≥2 planned code
  tasks), when requested by the user, when changed behavior affects
  authorization, money, data integrity, concurrency safety, or hard-to-reverse
  migration, or when a material correctness question remains uncertain. Cover
  unit-local invariants and cross-unit interactions within the [Evidence
  Contract](evidence-contract.md#local-completion) proof boundary (using the
  2-lens [Complementary
  Panel](../phases/implementation-review.md#complementary-panel) when
  triggered).
- **Structure-only review**: Selected when full review is untriggered and the
  candidate adds a workspace crate, dependency or crate feature, trait or layer,
  custom general mechanism, config key, or fallback/shim/parallel path; route to
  [Structure-only review](../phases/implementation-review.md#structure-only-review).
- **Direct completion**: Single-task or inline candidate matching neither
  trigger above; complete final validation directly and record the checked
  trigger signals in the Completion result.

## Lifecycle

The owner fixes one candidate, its authoritative inputs, phase adapter, and
evidence boundary. The reviewer keeps that boundary read-only, attempts to
falsify it, and returns [Review Result V1](../interfaces/review-result-v1.md).
Review never owns repair, integration, acceptance, or movement. It reuses
available evidence and investigates concrete defects within that boundary,
rather than starting a second verification project. Missing optional proof and
preferences are not blocking findings; real in-scope defects still must close.

Check newly adopted hard constraints against the [Evidence
Contract](evidence-contract.md), including their original source and scope.
For mechanical identity refreshes, use [Transition](transition.md)'s unchanged
semantic-scope rule instead of restarting an unaffected review.

Use a bounded delta recheck by the same reviewer when repair addresses only
that reviewer's anchored findings, leaves Outcome, Boundary, accepted inputs,
interfaces, and risk surface unchanged, and introduces no unrelated behavior or
writable owner. The reviewer remains read-only and does not prescribe or
perform the repair.

A repair closes the finding; a remedy the finding suggests is evidence. Before
adopting one, the repair owner weighs what it adds or trades away: an accepted
canonical pattern, measured throughput, lockfile or profile-projection cost,
and new structure. Every recheck, delta or fresh, also falsifies structure the
repair added against [Engineering](../../../AGENTS.md#engineering); a repair
that closes its finding through an unjustified crate, dependency, layer,
configuration key, or parallel path still fails.

For Implementation, batch verified findings per pass and allow at most **two
bounded repair rounds** for the same anchored defect class on one candidate,
retaining the reviewer for bounded delta rechecks when the adapter permits
continuation (or using one fresh delta-scoped reviewer otherwise). If a second
repair still leaves the defect unresolved or adds no discriminating evidence,
stop cycling and apply [Parent-Owned
Recovery](transition.md#parent-owned-recovery) (focused diagnosis,
`adjudicator-agent`, or `Blocked`); every confirmed blocking defect still must
close before `PASS`.

For non-Implementation reviews, allow at most one bounded delta recheck per
review result. If that still FAILs, one fresh reviewer inspects the repaired
candidate, or reopen if Outcome is invalid. Do not start a third cycle on the
same candidate. Parent-Owned Recovery still governs repeated findings without
new evidence; reviewer replacement does not restart that allowance.

If the same-reviewer identity is unavailable, one fresh review covers the
bounded repair delta and invalidated proof, not the original unit, unless the
repair expanded the risk surface.

Use a fresh reviewer when repair changes the acceptance boundary, invalidates
unaffected review reasoning, introduces a new interface or behavior, or expands
the risk surface.

The phase adapter owns lenses and threshold, while the artifact, ledger, or
[Transition](transition.md) owner decides movement.

## Route

| Fixed boundary | Adapter |
| --- | --- |
| Standalone research synthesis | [Research](../phases/research.md#review) |
| Completed specification | [Specification Review](../phases/specification-review.md) |
| Technical design (system and Rust architecture) | [Technical Design Review](../phases/technical-design-review.md) |
| Planning result | [Task Review / Readiness](../phases/task-review-readiness.md) |
| Implementation delivery (assembled ledger, single-unit, or standalone review) | [Implementation Review](../phases/implementation-review.md) |

A phase-owned complementary panel replaces the default reviewer only when its
trigger partitions one fixed candidate into named non-overlapping lenses and
defines one synthesis threshold. For one surviving material evidence-backed
reviewer conflict (across panel lenses, replacement review, or [Parent-Owned
Recovery](transition.md#parent-owned-recovery)), the synthesis owner may
dispatch `adjudicator-agent` for [Lane Result
V1](read-only-delegation.md#lane-result-v1) while keeping final synthesis
authority. Stop after one result for one fixed boundary.
