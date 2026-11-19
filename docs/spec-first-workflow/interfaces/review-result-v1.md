# Review Result V1

Every independent review returns:

```text
candidate: <fixed artifact revision, diff, tree, or commit>
verdict: PASS | CONCERNS | FAIL | NEEDS_PARENT | REVIEW_HANDOFF_INVALID
findings: <anchored surviving findings grouped by axis, or none>
evidence_boundary: <primary files, symbols, checks, or mutated branches actually inspected>
reopen_owner: <smallest owner or none>
```

Each surviving finding names its anchor, outcome impact, classification,
smallest owner, and evidence boundary. Missing mandatory proof is never
downgraded to a concern.

| Scope | Classifications | Allowed verdicts |
| --- | --- | --- |
| Plan / Research review | `PHASE_DEFECT` (local repair), `UPSTREAM_GAP` (reopen user or external owner), `CONCERN` (bounded downstream risk with risk, owner, and closing action) | `PASS \| CONCERNS \| FAIL \| REVIEW_HANDOFF_INVALID` (`PASS \| FAIL` only on zero-delta checks) |
| Implementation review | `TASK_DEFECT` (repair owning task), `INTEGRATION_DEFECT` (repair seam across tasks), `UPSTREAM_GAP` (missing/invalid accepted input), `FOLLOW_UP` (non-blocking) | `PASS \| FAIL \| NEEDS_PARENT \| REVIEW_HANDOFF_INVALID` |

Return `REVIEW_HANDOFF_INVALID` when a required artifact, boundary, or ref is
missing or the candidate diff is empty; fix the handoff and redispatch without
consuming a repair round.
