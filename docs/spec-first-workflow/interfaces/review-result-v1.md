# Review Result V1

Every independent review returns:

```text
candidate: <fixed artifact revision, diff, tree, or commit>
verdict: <phase adapter value>
findings: <anchored surviving findings grouped by axis for Implementation, or none>
evidence_boundary: <what was independently checked>
reopen_owner: <owner or none>
```

Each surviving finding names its anchor, outcome impact, classification,
smallest repair/reopen owner, and evidence boundary (grouped under explicit
per-axis headings for Implementation reviews). Record the attempted falsifier
and result, or why it is unavailable; missing mandatory proof is not downgraded
to a concern.

Implementation finding classification is closed:

| Kind | Meaning | Routing |
| --- | --- | --- |
| `TASK_DEFECT` | The candidate falsifies the current task packet or [Engineering](../../../AGENTS.md#engineering). | `FAIL`; repair the owning task. |
| `UPSTREAM_GAP` | An accepted input or dependency output is missing or invalid. | `NEEDS_PARENT`; reopen the smallest upstream owner. |
| `INTEGRATION_DEFECT` | A seam or assembly between implemented outputs fails. | `FAIL`; route repair under [Final Validation](../phases/implementation/final-validation.md#4-verify-findings-classify-and-route-repairs) to one fresh Lead (with combined seam writable scope), preserving unaffected implementation and valid final evidence. Reopen Planning only when accepted behavior or ledger scope must change. |
| `FOLLOW_UP` | An improvement is outside the current packet and does not falsify it. | Never blocks `PASS`; route separately through Planning if accepted. |

Non-implementation adapters use `PASS | CONCERNS | FAIL`. Implementation uses
`PASS | FAIL | NEEDS_PARENT`; `NEEDS_PARENT` also names the unverified claim
and unavailable proof/action owner. A missing boundary or empty diff returns
`REVIEW_HANDOFF_INVALID` without a verdict and names the Delivery Owner that
must repair the handoff.
