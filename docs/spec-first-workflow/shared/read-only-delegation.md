# Read-Only Delegation

Delegate a bounded read-only question when separate context improves focus,
independent investigation can challenge a material assumption, or concurrent
work can shorten completion. The expected benefit must justify briefing,
source loading, and synthesis.

A question must support a named parent decision or required evidence result,
have a checkable read-only evidence boundary, and be independently answerable
from the supplied inputs without depending on in-flight mutation or reasoning.
The root must be able to verify and synthesize the result. Keep user intent,
authorization, ordered reasoning, synthesis, repair, integration, acceptance,
and completion in the root. One lane owns one eligible question at a time; repeating the
same question over the same evidence is not coverage.

Before dispatch in a role-carrier harness, load [Agent Harness](../../agent-harness.md);
brief the lane with one outcome, authoritative inputs, mode, read-only boundary,
and stop condition. When an
expensive-to-reverse decision has a real open fork, use [Generative Candidate
Lanes](#generative-candidate-lanes).

Evidence lanes return [Lane Result V1](#lane-result-v1), domain
decisions [Decision Result V1](#decision-result-v1), generative candidate
lanes [Candidate Proposal V1](#candidate-proposal-v1), and
independent reviews [Review Result V1](../interfaces/review-result-v1.md). Evidence
may expose a new owner decision, but a lane does not expand scope or become the
authority it inspected. The root consumes gaps through [Parent-Owned
Recovery](transition.md#parent-owned-recovery).

Check the returned result against the delegated question and its evidence
boundary. Inspect primary sources for decision-critical claims, conflicts,
and gaps; expand inspection when those checks expose uncertainty. Reuse
supported findings instead of repeating the lane's full investigation.

## Generative Candidate Lanes

Use only when a hard-to-reverse choice has a real open fork and independent
candidate construction can reduce anchoring. Give each lane a fresh context
window with the same accepted drivers, constraints, evidence boundary, and
output interface without exposing another candidate, and assign each lane a
**distinct, contrasting optimization pressure** so parallel lanes explore
genuinely different architectures instead of collapsing onto one median answer.
The root compares only viable same-level substitutes against the same drivers,
synthesizes compatible elements, and owns the selection. Candidate count is
evidence-driven, not a ceremony. For Technical Design forks, load [Design It
Twice](../phases/technical-design/design-it-twice.md).

## Lane Result V1

Every material read-only evidence lane returns:

```text
conclusion: <answer or no supported conclusion>
evidence: <strongest primary locator and result>
uncertainty_or_conflict: <gap or none>
disposition: accept | reject | repair | carry_as_proof_or_risk | reopen
destination: <artifact or owner>
```

The result is a secondary source. Carry the primary locator, not a transcript
or unsupported restatement.

## Decision Result V1

Return one domain decision record:

```text
disposition: decision | constraint_only | proof_only | no_new_decision_required | blocked
decision_or_constraint: <selected policy, constraint, proof obligation, or none>
forced_consequences: <affected paths and outcomes, or none>
proof_or_gap: <focused falsifier, current evidence, or explicit gap>
blocker: <missing owner input or none>
strongest_rejected_alternative: <required for decision, otherwise none>
rejection_reason: <evidence or constraint that rejects it, otherwise none>
reopen_owner_or_condition: <owner or observable condition>
```

Naming the strongest rejected alternative distinguishes a chosen mechanism from
an asserted one. Keep the record inside the selected domain.

## Candidate Proposal V1

Return one contrasting design candidate from a generative candidate lane without
picking a winner or comparing against sibling lanes:

```text
pressure: <assigned optimization axis>
proposal: <flow or Rust interface: types, methods, invariants, ordering, error modes, lifetimes>
caller_sketch: <3-8 line realistic call-site usage against existing workspace types, or entry->finality trace>
hidden_complexity: <what stays inside the module or mechanism behind the seam>
dependencies_and_cost: <dependency category/adapters, failure/cancellation/concurrency profile, budgets & unit cost>
thin_leverage_and_risks: <where this candidate bends, breaks, or pays extra cost>
blocker_or_gap: <hard constraint conflict, infeasible pressure, missing spec rule, or none>
anchors: <source-verified repo and Cargo.lock locators used, or uncached gaps>
```
