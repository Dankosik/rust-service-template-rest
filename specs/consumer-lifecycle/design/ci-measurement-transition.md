# CI measurement reopen transition

```text
status: ready
owner: Technical Design — scoped System / Integration Design reopen
result: specs/consumer-lifecycle/design/ci.md
review: specs/consumer-lifecycle/design/ci-measurement-review.md
movement_evidence: C1–C3 remain unchanged; the cache condition, final-source comparison boundary, alternatives, proof, cost and stop rules are closed and fresh Technical Design Review passed
reopen_owner: none
next_owner: existing Implementation Completion through the root continuation coordinator
```

## Ready decision and custody

[CI design](ci.md) is ready at SHA-256
`998c4994f5d81822ea6ca96fd549cf4142da8fc4690a0d8ea00ecb33b646bcb9`.
Its reopened measurement section selects ordinary native imports with the
shared `runtime-image` cache observed absent. It permits a narrow C2 claim only
from one fresh final matched serial/split pair, retaining all gates and artifact
proof. Warm performance remains unmeasured. No code, workflow, cache mechanism,
consumer, ledger or other design decision changed in this phase.

The old control run remains bound to `3fa14ecb728184cf8b44eb978287378d959004dd`
and source `8cf53b2818ffdb55320cd5fafeb7e9a75129a3d8`. Preserve it as exact
old-candidate proof and a bounded cost/cache diagnostic. No split counterpart
is required merely to finish that superseded final-candidate comparison.
Current repaired code was `a3853f32b1fb8374b42cf1a6e75176f9bb19165e` at the
review boundary. During review Completion advanced to
`6525aaf03c54137181be3ea7bccba4475f63f7f0`, changing only the verify fixture's
copy to include `make/profile-postgres.mk`. Static inspection of that delta
confirms another executed quality-input repair; it reinforces the same final
freeze requirement and changes no selected measurement mechanism. Neither
revision is declared final F here. Completion freezes F after integration.

Evidence used for the cache decision:

| Snapshot | SHA-256 | Observation |
| --- | --- | --- |
| [Before old-control dispatch](../completion-records/ci/consumer-lifecycle-cache-before-8cf53b.json) | `e8e07d59d5dab683e4b7b3bfdf088908a0a6bba5be1ee4e4e17a78dff3effa53` | 15 entries; 10,479,073,385 bytes; no runtime-image entry |
| During old control, `/tmp/consumer-lifecycle-cache-during-8cf53b-2106.json` | `a621d2d91a853bab4c9eb5018bed1c583e111c4bcea3100b5890daf14499cfe4` | Same count/bytes and absence; not complete import/hit proof |

The root was given the temporary snapshot locator for retention under existing
Completion evidence custody. Snapshot absence is not a final cache claim; the
selected actual-log and per-arm evidence rules remain mandatory.

## Next action and stop condition

The root remains sole continuation coordinator and Ledger Writer; existing
Completion owns integration, actual final CI/measurement and acceptance.
Before any new comparison dispatch it retires warm-only instructions in the
[earlier preparation plan](../completion-records/ci/comparison-plan.md), collects
the old control's terminal diagnostics, resolves any blocking proof failure,
freezes F and constructs the matching temporary serial control. It refreshes
included quota, cache availability and all required execution inputs, then runs
at most one pair under the unchanged effects/cost envelope. Final CI still
belongs to the candidate actually executed.

Failed matching, missing required proof, changed cache behavior, noise, no whole
selected-gate reduction or exceeded cost returns the exact evidence gap to the
CI Design owner; C2 remains incomplete. There is no blanket retry, warming,
cache-deletion, paid-runner or increased-storage permission. Unaffected A/B,
native recovery and local preparation continue. No user technical choice,
Planning reopen, new acceptance unit or new user-visible task is needed.

## Proof and phase boundary

The phase loaded current Transition, System / Integration Design, Technical
Design Review, Agent Harness/Codex, artifacts/lifecycle and scoped delivery
owners. The harness and phase owners match the primary checkout. User-provided
current AGENTS instructions govern the unrelated local-policy drift; no policy
files were copied or changed.

Static consistency and whitespace checks passed. Native scoped `make docs-check`
for the design passed before independent review: 11 total links, 10 unique,
4 OK, 7 excluded, zero errors, plus image-input watch coverage. The final check
including these two receipts is recorded below. This is local relative-link
and fragment proof, not live measurement or external-link validation.

Fresh independent review returned PASS with no findings; its actor completed.
The phase performed no Rust build, native CI dispatch, cache mutation, remote
write or consumer operation. This ready transition closes technical input only;
C2, final implementation acceptance and the wider outcome remain with Completion.

Final native documentation check passed for all three scoped files: 21 total
links, 17 unique, 12 OK, 9 excluded, zero errors; image-input watch coverage
passed. The check used the pinned offline lychee image through `make docs-check`.
Recording this result adds no links or fragments. No remaining phase-owned
process or descendant is running.
