# Candidate-history input and C2 recovery transition

```text
status: ready
owner: Technical Design — bounded gate-input and C2 recovery disposition
result: specs/consumer-lifecycle/design/ci.md
review: specs/consumer-lifecycle/design/ci-history-review.md
movement_evidence: Root-selected candidate-history scope, native enforcement, repair owner and evidence/cost limits are fixed; fresh narrow review and one bounded precondition recheck passed
reopen_owner: none
next_owner: existing Implementation Completion through the root continuation coordinator
```

[CI design](ci.md) is ready at SHA-256
`68bb2fcc518c520284d335d1a01e4895a9a528ba743459af9ade0a9024186ee5`.
The original scheduling/cache/artifact mechanism and C1–C3 remain unchanged.
The new decision closes the history input of candidate admission: native
Gitleaks over actual `HEAD` and every reachable ancestor, complete Git history,
unchanged rules/ignores/redaction and failing result. The existing history
target refuses with exit 2 unless the native shallow-state query returns
`false`; CI retains full fetching. Repository-wide `--all` audit stays a
separate explicit native CLI action. No broad exclusions or gate waiver exist.

## Next owner and proof

Completion returns the bounded source repair to the existing unit owner under
final-validation repair. Its surface is `make/template.mk`'s history target,
the `secrets` workflow call/comment and matching command/security documentation,
with only necessary existing routing parity. It proves reachable-history
rejection, unrelated-ref independence and shallow-history refusal using bounded
native evidence, then applies the required final validation/review and normal
CI to the actual repaired candidate. This phase edited no code or runtime input.

The completed serial `37541180687` remains successful evidence for its actual
control/source. Split `37546407132` retains its actual failed secrets job and
every later producing-attempt/terminal result. Native readback established that
the two finding commits are outside candidate/control ancestry and in another
fetched branch. The supplied serial/split history counts differ, 451 versus 456.
The gate repair changes executed inputs and cannot refresh old whole-workflow
receipts into new-candidate proof.

C2 is incomplete. After the fixed review, the coordinator additionally reported
successful split source-image commands on runner image `20260927.320.1`, versus
serial source-image runner `20261004.327.1`. Those observations belong to
Completion's exact receipts; the generic mismatched-environment rule already
covers them. No observed build-duration difference is attributed to scheduling.
No terminal split conclusion is inferred while independent jobs remain running.

Count all earlier diagnostics, failures, replacement work and current arms.
The spent pair and prior recovery allowance grant no further split retry or
benchmark cycle. Normal repaired-candidate CI remains within the existing
zero-cost standard public template CI authority, without acquiring comparative
performance meaning. Future C2 measurement returns to the CI Design owner for
a new bounded disposition after repair and input/environment admission.
Unchanged consumer/native-recovery work may continue; new consumer remote
authority remains absent. No user technical choice or new task is needed.

## Phase evidence

A fresh narrow reviewer passed the new gate-scope delta, then passed the sole
bounded shallow-precondition recheck; neither performed implementation or
acceptance. Both phase descendants completed. Static whitespace/consistency
checks passed. Before final receipt creation, native scoped `make docs-check`
passed for the CI design: 13 links, 12 unique, 4 OK, 9 excluded, zero errors,
plus image-input watch coverage.

This is local documentation proof, not proof that the next committed candidate
contains every referenced file. The coordinator integrates these three scoped
files together and retains its committed-tree closure check. All relative links
introduced here point only to these scoped design files. The phase performed no
build, Gitleaks scan, CI dispatch, cache/ref mutation or external write.

Final scoped native documentation check passed for all three files: 18 total
links, 15 unique, 6 OK, 12 excluded, zero errors, with image-input watch coverage.
Recording this result introduces no links or fragments.
