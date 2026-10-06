# Technical Design Transition

```text
status: ready
owner: Technical Design
result: specs/jobs-reliability-completion/design/system.md + design/ownership.md
review: specs/jobs-reliability-completion/technical-design-review.md — PASS
movement_evidence: R1–R5 mechanisms, material flows, durable truth, failure/enforcement boundaries, finite workload, fixture placement, source-only carrier and executed upgrade path are closed; independent review found no material gap
reopen_owner: none
next_owner: Planning
```

Consume the [system design](design/system.md), [ownership map](design/ownership.md),
[review](technical-design-review.md) and ready [specification](spec.md).
Checkout: `/Users/daniil/.codex/worktrees/jobs-reliability-followup/rust-service-template-rest`.
Branch: `codex/jobs-reliability-followup-20261005`; existing draft PR #240.
Source baseline is `ac88395be87cba3a1e0587f533dc50a71e358c8d`;
Definition-only HEAD at design/review was `ccf16c3ca29955701639ce3f522626228c699b75`.
Root remains continuation coordinator; this actor owned only Technical Design.

## Closed decisions and next action

R1 extends existing attempt/engine custody with safe handler destruction and
a pre-submission supervisor guard feeding `Started::failed`. R2–R5 use one
fixture-owned reading-counter feature: atomic acceptance of three intents,
separate marker/aggregate, real worker, independent receiver DB, real
crash/backup/restore, explicit unknown hold and an initialized derived service.
R4 fixes three ordinary plus one publisher slot, pool eight and the accepted
5/180/300-second fault/recovery/runtime envelope. R5 performs portable sync and
explicit source adoption between baseline and the exact eventual candidate SHA.

Planning can partition these existing owners without inventing mechanism,
runtime policy or business meaning. Concrete cases/assertions/commands remain
Implementation decisions. One final matching build and required real evidence
run under the shared validation owner; no per-task review or repeated full
build matrix is introduced. The source reference rehearsal runs once, with one
initialized all-required-profile representative; existing profile proof remains.

No production code, test implementation or runtime migration was changed in
this phase. No heavy runtime/DB/crash/restore/load/upgrade proof was run or
claimed. The phase proof is static consistency plus independent design review;
final `make docs-check` passed (1984 total, 1164 unique, 1146 OK, zero errors),
including review and Transition links. `git diff --check` passed; the newly
added design/review/Transition files also received a direct whitespace check.

Authority carries forward: all five accepted improvements, local edits and
bounded real proof, commit/push and PR create/update. Merge, production effects,
purchases and new cloud resources remain outside the accepted scope. Preserve
unrelated edits. No user-owned technical choice is outstanding.

Reopen Specification for changed finality/replay/behavior; Research for a
source contradiction; Technical Design for an unavailable mechanism, placement
gap, measured envelope violation or unsupported reference API. A new dependency,
public endpoint, generic framework or runtime knob is not implied by this
handoff. The narrow stale HeaderValue prose correction retains existing behavior.
