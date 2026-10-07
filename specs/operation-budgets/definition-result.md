# Definition result: operation budgets

```text
status: ready
owner: Definition (Intake, Specification, Specification Review)
result: specs/operation-budgets/intent.md and specs/operation-budgets/spec.md
review: Review Result V1 below
movement_evidence: requester meaning and all material behavior have dispositions; independent review PASS; current scope is ready for mechanism and ownership decisions
reopen_owner: none
next_owner: Technical Design — System / Integration Design and Rust Code / Ownership Design
```

Authoritative inputs: [Intent](intent.md), [Specification](spec.md), and
[supporting evidence](research/baseline.md). Current base is
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af` in worktree
`/Users/daniil/.codex/worktrees/operation-budgets/rust-service-template-rest`,
branch `codex/operation-budgets-20261005`. Current repository workflow owners
were read from that base. Only the four task-local Definition files changed.
No source, configuration, dependency, or infrastructure change was made here.

## Review Result V1

Initial independent reviewer: `/root/budget_definition/spec_review`. The
bounded B1/B5 clarification was independently reviewed by
`/root/budget_definition/late_finality_review`. Both were dispatched with fresh
history as `reviewer-agent`, native model `gpt-6-astra`, effort `high`.

```text
candidate: fixed Definition at base 78aa3a832bfb4d7e9632ce5ebbbf1680705c31af
verdict: PASS
findings: none
evidence_boundary: initial fixed artifact review plus bounded F1 repair, retained for unchanged scope; fresh bounded B1/B5 known-finality clarification review against unchanged intent/baseline and adjacent B1/B5/B6; no implementation/runtime/design-completion claim
reopen_owner: none
```

Reviewed hashes:

| Artifact | SHA256 |
| --- | --- |
| intent.md | `3355834cecee955ebe3768a49e6132f3ac5b4dc69d9ab7ed8ffa1336baae0481` |
| spec.md before status promotion | `bc2691898b1e0a421033c288183b98032fef98fcdc013f9b92fc06aa4cc1ce8d` |
| research/baseline.md | `8043189320f73de6425c79456206efe3c31340aa3d3e8b2b4848b521e5bb6ea1` |

The only post-review candidate edit changes `Status: draft` to `Status: ready`.
The current spec hash is
`02c087f230e579c3bf3eecd20237692e9fe5038293befd29ef578993db330de1`.
This mechanical status promotion preserves the reviewed semantic boundary.

The initial review found F1: OAuth accepted the existing `OpeningOnly` client,
but B4 generalized the default `FullRpc` lifetime. The repaired B4 retains both
policies while charging credentials to the selected interval from composition
entry. The bounded recheck attempted an unbounded-lifetime `OpeningOnly`
stream, a caller deadline longer than opening, a shorter parent, cached
credentials, and default `FullRpc`. The reviewer found no surviving divergence.
Earlier review reasoning on cancellation isolation, body ownership, and
unknown-effect preservation remained valid.

Technical Design exposed a narrow B1/B5 interpretation conflict: whether a
definitive mutation result from a live-started synchronous SDK poll must become
an error solely because its return crossed the cutoff. The clarification keeps
the existing adapter success for that known completed effect and keeps the
original terminal owner's deadline/settlement policy. An internal success
does not authorize late terminal success. Pending expiry remains unknown;
known rejection remains known; no subsequent poll/dispatch is allowed after
stop is observed and no new retry eligibility is introduced. A new
known-applied storage error is not a required behavioral change.

The fresh reviewer returned PASS after challenging late terminal success,
erased confirmation/replay, pending-versus-completed finality, additional work
after stop, and standalone budget reset. This closes accepted meaning only.
Technical Design must make terminal enforcement and adapter outcome retention
consistent in its own mechanism and review. Intent, scope, durations, and
external authority remain unchanged; no new error code or latency SLA was
added.

## Movement and proof boundary

Definition resolves behavior only. Existing defaults stay baseline limits;
default `FullRpc` and explicit `OpeningOnly` retain their distinct meaning.
HTTP/gRPC opening completion is distinct from response-body completion. S3
resource lifetime is finite even when a retained consumer does not poll.
Current retry, immutable identity, and uncertain-finality owners remain intact.

Static validation: `make docs-check` passed with zero broken links, and
`git diff --check` passed. That link proof is reused for the B1/B5 prose-only
clarification, which changes no link target. No Rust build, tests, live provider observation,
CI, commit, push, PR creation, merge, or deployment is claimed by this phase.
Those accepted delivery obligations stay with the continuation coordinator.

Technical Design chooses the common carrier/placement, extraction and stream
phase ownership, adapter integration, error projection, and supported S3
resource-termination mechanism. Compare existing library mechanisms only where
that decision needs evidence. Preserve the effect/finality and retry owners.
Open reference PRs remain evidence to inspect, not authority to import whole
patches or omit required behavior from this separately based candidate.

There is no open requester-owned input. The explicit assumption is preservation
of existing generic transport stream policy; reopen Intake only if desired
behavior changes to a new global stream-duration policy. Reopen Specification
for an actual behavioral conflict, supporting Research for invalid factual
evidence, and Technical Design for mechanism/ownership gaps. Continue through
the root coordinator; this phase actor stops at the reviewed Definition.
