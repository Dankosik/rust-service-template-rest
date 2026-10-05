# Technical Design transition

- status: ready
- owner: Technical Design (`/root/technical_design`)
- result: [Selected design](design/selected-design.md) and
  [Ownership Map V1](design/ownership.md)
- review: [Technical Design Review](technical-design-review.md), PASS; initial
  full review plus fresh bounded final NATS source-selection review, consuming
  the [ownership panel](design/ownership-review.md)
- movement_evidence: S1–S5 mechanisms, current API/source evidence, material flows,
  cancellation/failure ownership, finite native reserves and exact file owners
  are closed; no product or technical decision remains for Planning to invent.
  S6 has named existing guide/recipe owners. Implementation selects test cases.
- reopen_owner: none currently; Technical Design for contradicted mechanisms or
  placement, Definition for changed observable behavior/policy, Research for
  contradicted version/source evidence.
- next_owner: Planning, dispatched by the current root continuation coordinator

## Fixed handoff

Worktree: `/Users/daniil/.codex/worktrees/buffer-resource-bounds/rust-service-template-rest`.
Branch: `codex/buffer-resource-bounds-20261005`.
Unchanged implementation source HEAD: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
This phase wrote only Technical Design/review/transition documents. No source,
dependency, task-ledger, commit, remote, provider or infrastructure mutation occurred.

Current artifact SHA256:

| Artifact | SHA256 |
| --- | --- |
| spec.md | `cace772a9cd747bfe414855f5520160960407dc6998aac8cffd472633b52b507` |
| research/dispositions.md | `4534602d30635c993e40e21726ad501e7bdcdd8c0f40462de6e6bbcbcf1d3673` |
| design/selected-design.md | `7ad5296d455d8fb39256bbed86b1273148943c2fa67e73b7a33d2e452bb2e936` |
| design/ownership.md | `d4a6bebde0bcdefa287bf74078629faad994d88cf2168999ba560cb1bf2a5bec` |
| technical-design-review.md | `6f0d4f898a7073680226911e1129951345a38a252d0633fcc24b457dda1260d5` |

The final ready design differs from its reviewed hash only by lifecycle status
and the explained obsolete-wording correction. Retained ownership receipts cover
unchanged scope; the final fresh review covers R6's smaller native-source delta.

## Decisions Planning must carry

- Object storage response envelope: client non-2xx interception precedes SDK checksums; four
  nonstreaming control-operation success interceptors share the native limiter.
  Successful GET remains unchanged; actual DATA is counted, not HEAD metadata.
- JSON preparation: one bounded counting/discarding writer per current optional
  owner, retaining exact length/error order and no messaging-only jobs dependency.
- Redis: immediate 256 application admission, one separate external probe, current
  single supervisor maintenance; possible-dispatch cancellation retires that
  generation before permit reuse, using existing recovery and no write replay.
- NATS: same-version vendor0.50.0 adaptive handler pruning from upstream draft1629
  at `7db17cf15830a1a65e7ba73cecda65aee72b1ea7`, plus retained-borrow ACK polling.
  Native P admission/parser/recovery remain; no custom receiver wrapper/manager.
  Distinguish live P ownership, finite stale metadata, native buffers and DLQ's
  broker-dependent wire ceiling. Source custody, profile removal and existing
  dependency/image/initializer routing belong to the same implementation outcome.
- One assembled final validation and delivery review. Existing CI-owned optional
  integrations remain CI-owned; no per-task build/review or additional environment
  matrix is imposed. Follow the current repository validation budget.

## Proof and authority boundary

Evidence is pinned-source and static design review. Scoped artifact relative links
and whitespace were checked; source HEAD remains unchanged. No build, test,
benchmark, allocation measurement, live provider call, CI or implementation
correctness is claimed. The narrow same-version NATS patch still needs source
integrity, native lifetime and adapter parity proof during Implementation.

Current worktree AGENTS.md and workflow/harness owners govern the next phase.
Retain requester tool routing in fresh descendants: shell commands start with
`/opt/homebrew/bin/rtk` (`proxy` for exact unfiltered output), and CodeGraph uses
this absolute worktree as projectPath. Fresh actors consume current instructions,
not a stale inherited checkout's policy.

The user's authorized continuation remains project-necessary fixes, local
validation, commit/push and one separate PR. No merge, deployment or infrastructure
changes are authorized. This actor stops at the reviewed Technical Design boundary;
the root continues Planning without another technical confirmation.
