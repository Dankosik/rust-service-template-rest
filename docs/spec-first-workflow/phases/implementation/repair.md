# Repair Assignment

Fix one batched set of confirmed final-verification or review defects as a fresh
`ACCEPTANCE_UNIT_LEAD` inside the granted writable scope, then return an
[Implementation Result](unit.md#implementation-result).

## Steps

1. **Confirm each finding against live code.**
   - Read every finding, affected packet, prior `decisions`, and (on Round 2)
     the prior attempt's hypothesis and failure. Load `rust-debugging` for an
     uncertain/async cause or a Round 2 attempt, stating 2–3 ranked falsifiable
     hypotheses before editing.
   - Verify each finding by kind before touching production code:
     - *Failing check or test:* run the exact failing command and observe `red`.
     - *Behavioral, runtime, or seam counterexample:* verify reachability in
       live callers and write or reuse a minimal seam regression test that goes
       `red` first.
     - *Structural violation:* confirm callers and [Engineering](../../../../AGENTS.md#engineering)
       constraints while keeping existing seam tests `green -> green`.
     - *Weak or tautological test:* confirm the named surviving mutant and
       strengthen the seam test so the broken branch goes `red`.
   - **Refutation bar:** refute a finding only when primary code or a direct
     check disproves the exact counterexample path (unrelated passing tests do
     not refute it). Leave refuted paths untouched and record the refuting
     anchor in `decisions`. If every finding is refuted, return `Blocked`
     (`next_owner: Orchestrator — refuted findings`).
   - *Done when:* every finding is either reproduced (`red`) or refuted with a
     primary anchor.

2. **Repair root causes and re-verify (`green`).**
   - Fix each confirmed root cause cleanly inside the granted scope (reconciling
     both sides of a seam for an `INTEGRATION_DEFECT`), weighing any suggested
     remedy against [Engineering](../../../../AGENTS.md#engineering) so the fix
     adds no unjustified structure.
   - Re-run every reproducer and follow [Unit Implementation](unit.md#steps)
     Step 3 for the repaired surface, recording repaired root causes,
     refutations, and invalidated checks in `decisions`.
   - Escalate scope expansions, ungranted crates, or exposed `UPSTREAM_GAP`s
     under [Unit Escalation](unit.md#escalation).
   - *Done when:* every confirmed defect is `green`, self-audited, and returned
     in [Implementation Result](unit.md#implementation-result).
