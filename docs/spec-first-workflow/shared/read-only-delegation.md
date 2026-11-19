# Read-Only Delegation

Delegate a read-only question when fresh context protects the smart zone,
independent scrutiny challenges an assumption, or parallel scans save time.

- **One bounded question per lane:** pass the exact question, authoritative
  inputs, read-only boundary, and expected output format (load [Agent
  Harness](../../agent-harness.md) when dispatching in a role-carrier harness).
  Keep synthesis, scope, and final decisions in the parent; never repeat the
  same question over unchanged evidence.
- **Verify decision-critical claims directly:** every lane result is a secondary
  source—check primary locators for decision-critical claims, conflicts, and
  gaps, reuse supported findings instead of redoing the scan, and handle gaps
  through [Parent-Owned Recovery](transition.md#parent-owned-recovery).
- **Match format to role:** `evidence-agent` and `adjudicator-agent` return
  [Lane Result V1](#lane-result-v1); `specialist-agent` returns [Decision
  Result V1](#decision-result-v1) for a domain question or [Candidate Proposal
  V1](#candidate-proposal-v1) for [Design It
  Twice](../phases/technical-design/design-it-twice.md); `reviewer-agent`
  returns [Review Result V1](../interfaces/review-result-v1.md).

## Lane Result V1

```text
conclusion: <answer or no supported conclusion>
evidence: <strongest primary locator and result>
uncertainty_or_conflict: <gap or none>
disposition: accept | reject | repair | carry_as_proof_or_risk | reopen
destination: <artifact or owner>
```

Carry primary locators rather than transcripts or unsupported restatements.

## Decision Result V1

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

## Candidate Proposal V1

Return one contrasting design candidate without picking a winner or comparing
against sibling lanes:

```text
pressure: <assigned optimization axis>
proposal: <flow or Rust interface: types, methods, invariants, ordering, error modes, lifetimes>
caller_sketch: <3-8 line realistic call-site usage against existing workspace types, or entry->finality trace>
hidden_complexity: <what stays inside the module or mechanism behind the seam>
dependencies_and_cost: <dependency category/adapters, failure/cancellation/concurrency profile, budgets & unit cost>
thin_leverage_and_risks: <where this candidate bends, breaks, pays extra cost, or locks in one-way doors>
blocker_or_gap: <hard constraint conflict, infeasible pressure, missing spec rule, or none>
anchors: <source-verified repo and Cargo.lock locators used, or uncached gaps>
```
