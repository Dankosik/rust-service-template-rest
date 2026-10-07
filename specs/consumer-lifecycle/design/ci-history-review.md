# Candidate-history input and C2 recovery review

```text
candidate: design/ci.md SHA-256 65bfecb822615480844ef96c8d945b09450ddbdf721faf2858e8317f0a83c6de; two added history-input/current-disposition sections against fb33186d0eb1fab14ff37d071a4e2d6eac9bb45c
verdict: PASS
findings: none
evidence_boundary: Fresh narrow Technical Design Review followed by one bounded shallow-history precondition recheck; no implementation or acceptance claim
reopen_owner: none
```

Fresh reviewer: native `/root/consumer_ci_design_reopen/ci_history_review`,
`reviewer-agent`, explicitly dispatched with `gpt-6-astra`, `high` and
`fork_turns: none`. Native state confirmed execution and both terminal results.
The reviewer owned no edits, builds, scans, effects or movement.

The first fixed delta, SHA-256
`1f557ade0d735282cf52452eab67ca2df9cc9db9fea1e67f775f1b9539ce7b94`, passed.
The sole recheck verified the added refusal of shallow/unavailable history;
removing only that clarification reproduced the original reviewed hash.
After PASS, status-only promotion produced ready [CI design](ci.md) SHA-256
`68bb2fcc518c520284d335d1a01e4895a9a528ba743459af9ade0a9024186ee5`.

## Attempted falsifiers

- **Candidate reachability weakens the accepted history scope.** Not established.
  The root explicitly selected complete candidate ancestry for admission,
  including merged-parent ancestry. Native `HEAD` selects that ancestry while
  preserving pinned Gitleaks rules, ignores, redaction and failure behavior.
  This is a scope decision, not a claim that the scanner detects every possible
  secret. The [pinned scanner implementation](https://raw.githubusercontent.com/gitleaks/gitleaks/v8.30.1/sources/git.go)
  passes log options to native Git; [Git revision semantics](https://git-scm.com/docs/gitrevisions#_specifying_ranges)
  define reachability.
- **Shallow history is passed off as complete.** Rejected by the pre-scan
  requirement that `git rev-parse --is-shallow-repository` return `false`;
  shallow or unavailable repository state refuses with exit 2. CI keeps
  `fetch-depth: 0`. [Git documents the boolean query](https://git-scm.com/docs/git-rev-parse).
  This is a required implementation boundary, not observed enforcement.
- **The change is an unowned waiver.** Not established. The design calls it an
  explicit policy correction, keeps broad native `--all` auditing separate,
  adds no exclusions, and assigns Make/workflow/documentation repair and final
  proof to Completion. The existing range path remains intact.
- **Old proof or failed timing is relabelled.** Not established. C2 stays
  incomplete; original source/run/attempt identities remain attached to every
  retained observation. A repaired scan or later CI cannot complete the failed
  split's successful whole-gate timing. Runner-image mismatch remains an
  independent comparison limit.
- **Recovery allocates another benchmark loop.** Not established. The prior
  pair/replacement allowance is exhausted. Normal repaired-source CI remains
  distinct from a future newly bounded comparison disposition.

## Evidence and limits

The reviewer independently checked C1–C3, the bounded diff/hash, current
Make/workflow selection, the redacted split-failure log and all four ancestry
checks for the two finding commits against candidate/control. Each ancestry
check returned exit 1 without errors; containing refs identify the separate
transport-recovery branch. The existing reviewer supplied custody observations
only; its reused context was not treated as an independent verdict on the new
gate scope.

Serial success/451 commits and corresponding runner-image differences were
supplied execution evidence; this phase did not refresh those native runs.
No final split outcome, push time, implementation proof, future cost or C2
improvement is inferred. Completion retains actual repair and proof obligations;
the CI Design owner retains any future bounded measurement disposition.
