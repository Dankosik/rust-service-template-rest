# Definition transition

- status: ready
- owner: Definition (`/root/definition`)
- result: [Intent](intent.md), [Specification](spec.md),
  [audit dispositions and evidence](research/dispositions.md)
- review: [Specification Review Result](definition-review.md), PASS; current fresh
  S2 delta review plus the retained unaffected initial review
- movement_evidence: the nonstreaming 2xx collection bypass is closed by revised
  S2 and fresh independent review; every material recommendation has an accepted
  disposition. The S5 same-version source-policy/evidence refresh changes no
  accepted behavior and retains prior semantic review under Transition.
- reopen_owner: none currently; Definition for changed behavior/policy, supporting
  Research for contradicted source facts.
- next_owner: the active Technical Design owner consumes the bounded S5
  evidence/source-policy refresh, retaining the current S2 contract

## Handoff boundary

Worktree: `/Users/daniil/.codex/worktrees/buffer-resource-bounds/rust-service-template-rest`.
Branch: `codex/buffer-resource-bounds-20261005`.
Baseline and unchanged source HEAD: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
The Definition files are local uncommitted artifacts; no implementation occurred.
Instructions: current worktree AGENTS.md, docs/spec-first-workflow.md,
docs/agent-harness.md, docs/agent-harness/codex.md and docs/prompt-composition.md.
Their workflow revision is the baseline above. Commit/push/one separate PR remain
authorized later; no merge, deployment or infrastructure effects are authorized.

Technical Design must close mechanism and placement for S1–S5: cancellation-safe
Redis generation/admission ownership, native NATS publication admission and retained
ACK/driver lifetime, the bounded JSON sink with exact count/error precedence, and
SDK interception before all currently used nonstreaming S3 reply collection and
GET error collection, without touching successful GET streaming/checksums.
S6 is targeted existing-guide/example work, not a new runtime memory policy.
No outstanding user-owned or Definition decision remains. If a mechanism cannot
meet these invariants, reopen the smallest affected requirement before implementation.

Static checks performed: artifact relative links resolve and git diff --check
reports no whitespace errors. No build, test, benchmark, provider call or CPU-heavy
validation ran. Final validation belongs to the assembled implementation owner;
this receipt is only readiness for Technical Design.

Current ready spec SHA256: `cace772a9cd747bfe414855f5520160960407dc6998aac8cffd472633b52b507`. Research SHA256:
`4534602d30635c993e40e21726ad501e7bdcdd8c0f40462de6e6bbcbcf1d3673`.
The only current delta is admission of the evidence-backed same-version async-nats
source lifetime repair when native controls cannot meet unchanged S5. Exact
mechanism, source provenance, proof and removal condition stay with Technical Design.
No new user-owned decision or Definition behavior blocker remains.
