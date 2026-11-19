# Review Result V1

Every independent review returns:

```text
candidate: <fixed artifact revision, diff, tree, or commit>
verdict: <phase adapter value>
findings: <anchored surviving findings grouped by axis for multi-axis reviews, or none>
evidence_boundary: <what was independently checked>
reopen_owner: <owner or none>
```

Each surviving finding names its anchor, outcome impact, classification,
smallest repair/reopen owner, and evidence boundary (grouped under explicit
per-axis headings for multi-axis reviews). Record the attempted falsifier and
result, or why it is unavailable; missing mandatory proof is not downgraded to
a concern.

Non-implementation finding classification:

| Kind | Meaning | Routing |
| --- | --- | --- |
| `PHASE_DEFECT` | The candidate falsifies an assigned check within the current phase. | `FAIL`; repair in the owning phase. |
| `UPSTREAM_GAP` | An accepted input, premise, or external/requester decision is missing, contradicted, or undelegated. | `FAIL`; reopen the smallest upstream owner (`Intake`, prior phase, or external authority). |
| `CONCERN` | A bounded downstream proof or external risk over an otherwise closed candidate. | `CONCERNS` (permits handoff without blocking movement; carry forward with risk, owner, and closing action). |

Implementation finding classification is closed:

| Kind | Meaning | Routing |
| --- | --- | --- |
| `TASK_DEFECT` | The candidate falsifies the current task packet, [Engineering](../../../AGENTS.md#engineering), or [Rust Change Surface](../../../AGENTS.md#rust-change-surface). | `FAIL`; repair the owning task. |
| `UPSTREAM_GAP` | An accepted input or dependency output is missing or invalid. | `NEEDS_PARENT`; reopen the smallest upstream owner. |
| `INTEGRATION_DEFECT` | A seam or assembly between implemented outputs fails. | `FAIL`; route repair under [Final Validation](../phases/implementation/final-validation.md#4-verify-findings-classify-and-route-repairs) to one fresh Lead (with combined seam writable scope), preserving unaffected implementation and valid final evidence. Reopen Planning only when accepted behavior or ledger scope must change. |
| `FOLLOW_UP` | An improvement is outside the current packet and does not falsify it. | Never blocks `PASS`; route separately through Planning if accepted. |

Non-implementation adapters use `PASS | CONCERNS | FAIL`, except that an
already-satisfied Definition result requires `PASS | FAIL`. Implementation uses
`PASS | FAIL | NEEDS_PARENT`; `NEEDS_PARENT` also names the unverified claim
and unavailable proof/action owner. For Implementation, a missing boundary or empty diff returns
`REVIEW_HANDOFF_INVALID` without a verdict and names the Delivery Owner that
must repair the handoff.
