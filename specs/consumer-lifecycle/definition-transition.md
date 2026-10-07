# Definition transition

```text
status: ready
owner: Definition
result: specs/consumer-lifecycle/spec.md
review: specs/consumer-lifecycle/definition-review.md — PASS
movement_evidence: All four accepted outcomes have grounded behavior, unchanged ownership, feasible native proof and explicit external boundaries; fresh independent review passed after its sole finding was repaired
reopen_owner: none
next_owner: Technical Design — System / Integration Design, followed by Rust Code / Ownership Design only where placement is not forced
```

## Authoritative inputs

- [Intent](intent.md) owns requester meaning and bounded assumption A1: the first
  consumer is a non-production demonstrator with synthetic data.
- [Specification](spec.md) owns release/run/rollback, consumer-preserving
  runtime updates, mixed-version/native recovery, and proof-preserving CI
  improvement. No numeric speed, RPO or RTO target was invented.
- [Supporting Research](research/synthesis.md) supplies existing owner facts,
  maintained/native alternatives, exact historical CI observations, provider
  caveats, limits and refresh conditions.
- [Review result](definition-review.md) owns the independent PASS evidence.

Worktree:
`/Users/daniil/.codex/worktrees/consumer-lifecycle/rust-service-template-rest`;
branch `codex/consumer-lifecycle-20261006`; source base
`699887b18594088a59bcc23a049d290d089f6da1`. Only
`specs/consumer-lifecycle/` changed in this phase. The source/runtime/CI
implementation is untouched. These local artifacts are not yet committed.

## Next decision and continuation

The root remains continuation coordinator and dispatches a fresh Technical
Design actor. It can proceed without inventing product meaning or requesting
technical confirmation. The design must close:

1. Reproducible old/new initialized baselines, legacy adoption, upgrade conflict
   and acceptance custody; reuse the existing projector and Git if they meet
   the contract, comparing Copier only against a demonstrated remaining gap.
2. The smallest real-consumer/release topology, a compatible distinct A/B image
   pair, and exact source-to-registry-to-runtime evidence custody.
3. Existing-harness native restore sequence and version pair, including jobs
   retention-owner activation ordering, broker restored identity and durable
   effect reconciliation. Keep production policy separate.
4. The image-gate critical-path strategy and its comparable native measurement,
   preserving selected shapes and total runner/cache cost visibility.

PR250 dependency candidate `2cb871895b9edd018205fc98223477e269fce2e9` is
reported integrated with main but still awaiting fresh independent integration
review and exact-head CI. The coordinator refreshes admission and supplies the
final ref before dependent implementation. Historical measurement at
`c3a3f18752ec8feee03a56a00b0edea2240d3b44` remains valid only for its original
scope; it is not current-head proof. Do not duplicate that dependency or the
separately completed guide correction.

## Concrete external preparation still required

Technical Design prepares the named synthetic consumer source and compatible
release pair locally. Before the coordinator seeks the missing external
authority, attach exact proposed repository owner/name/visibility, GHCR image
names and version/tag refs, existing action/attestation plan compatibility,
runtime destination, synthetic data and dependency composition, required
permissions, maximum cost/quota use, artifact retention and rollback/cleanup
consequences. Prefer an existing suitable local disposable runtime for the
rehearsal when it satisfies the accepted deployment observation; a new paid
host is not a requirement.

No remote consumer target or visibility, production target/data, spending
ceiling, or production recovery policy has been accepted. Those facts prevent
the corresponding effects, not independent local support work or this phase
transition. Do not replace the requested external proof with a guide or local
completion report. Publication and production promotion remain separately
bounded by their existing owners.

## Proof boundary and reopening

The phase ran static source/primary-document research and the native offline
Markdown-link check. No Rust builds, runtime tests, backup/restore experiments,
new CI runs, remote writes, publication or deployments were performed. Review
permits Definition movement only.

`make docs-check` passed before the final receipts (1,980 links, zero errors).
After the bounded repair and receipt creation,
`make docs-check MARKDOWN_FILES='specs/consumer-lifecycle/intent.md specs/consumer-lifecycle/spec.md specs/consumer-lifecycle/research/synthesis.md specs/consumer-lifecycle/definition-review.md specs/consumer-lifecycle/definition-transition.md'`
passed: 49 total, 43 unique, 23 checked OK, 26 external/excluded, zero errors.
This is offline relative-link/fragment validation, not external URL validation.

Reopen Definition for changed user scope or behavior, profile migration,
proof/security requirements, or a demonstrated infeasibility of the accepted
behavior. Reopen only Research or Technical Design for a technical/input gap.
External target, visibility, business recovery policy and cost/authority return
to the coordinator after the concrete independent preparation is complete.
