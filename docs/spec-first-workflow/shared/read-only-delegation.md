# Read-Only Delegation

Delegate a read-only question when fresh context protects the smart zone,
independent scrutiny challenges an assumption, or parallel scans save time.

- **One bounded question per lane:** pass the exact question, authoritative
  inputs, read-only boundary, and expected return format (load [Agent
  Harness](../../agent-harness.md) when dispatching in a role-carrier harness).
  Keep synthesis, scope, and final decisions in the parent; never repeat the
  same question over unchanged evidence.
- **Verify decision-critical claims directly:** every lane result is a secondary
  source—check primary locators for decision-critical claims, conflicts, and
  gaps, reuse supported findings instead of redoing the scan, and handle gaps
  through [Parent-Owned Recovery](transition.md#parent-owned-recovery).
- **Match format to role:** `evidence-agent` and `adjudicator-agent` return
  [Lane Result](#lane-result); `specialist-agent` returns [Specialist
  Recommendation](#specialist-recommendation) for a domain question or
  [Candidate Proposal](../phases/spec-and-design/design-it-twice.md#candidate-proposal)
  for `Design It Twice`; `reviewer-agent` returns [Review
  Result](review.md#review-result).

## Lane Result

```text
conclusion: <answer, narrowest defensible resolution, or no supported conclusion>
evidence: <exact primary locators, commands, and observed results>
gaps: <unresolved uncertainty, conflict, or missing owner input, or none>
```

Carry primary locators rather than transcripts or unsupported restatements.

## Specialist Recommendation

```text
recommendation: <policy, constraint, proof obligation, or blocked>
rejected_steelman: <strongest simpler substitute and exact invariant or bound it breaks, or none>
consequences_and_proof: <affected paths and outcomes, plus falsifier or primary evidence>
reopen_if: <objective reopen condition or missing owner input / blocker>
```
