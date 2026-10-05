# Planning transition

Transition Result V1:

```text
status: ready
owner: Planning
result: specs/process-lifecycle/plan.md (one fixed implementation unit)
review: specs/process-lifecycle/planning-review.md (PASS)
movement_evidence: Atomicity and written readiness walkthrough passed. L1-L8 deltas, exact writable owners, canonical/profile compatibility, replacements, final observable success, proof custody and known CI-owned categories are closed. No product, architecture, infrastructure or test-design input is missing before implementation.
reopen_owner: none
next_owner: Implementation
```

Current ready plan SHA256:
`435776fc32f5e265ec40e6c63f5a1480b48af9d87f854fdea8468391b09c7bd7`.
Review receipt SHA256:
`8bc393ddae0b4eab81a641900049b5b8c32e7f1809be7fc19d2aa12534687d8c`.
The plan differs from the reviewed candidate only by `draft` to `ready` status;
the accepted semantic scope is unchanged. Base and authoritative input hashes
are retained in the [plan](plan.md); the [review](planning-review.md) records
independent verification and falsifiers.

The continuation root can now dispatch a fresh native general-purpose
Acceptance-Unit Lead to this fixed unit using the repository
[Implementation owner](../../docs/spec-first-workflow/phases/implementation.md).
Use the same checkout
`/Users/daniil/.codex/worktrees/process-lifecycle/rust-service-template-rest`,
branch `codex/process-lifecycle-20261005`, and preserved base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. No separate App chat or persisted
task ledger is needed. The Lead implements code and tests, keeps execution
state in `specs/process-lifecycle/implementation.md`, joins writers, and returns
`Implemented`. Root assigns one assembled validation/review boundary and
retains the authorized single-PR publication and selected CI results.

Ready-to-send Implementation brief:

> Implement the fixed process lifecycle unit in
> `specs/process-lifecycle/plan.md`, consuming its ready specification and
> design. You own the plan's local production, documentation and test-writing
> surfaces in this checkout; you are not alone, so preserve other work and
> accepted phase artifacts. Choose concrete tests while coding and retain
> execution state in `specs/process-lifecycle/implementation.md`. Use the
> existing lifecycle owners and closed adapter APIs, keep all profile and
> compatibility obligations, and return Acceptance Result V1 `Implemented`
> with the bounded diff after joining writers. Publication and the assigned
> final validation/review boundary remain with the continuation root. Stop
> this assignment at that implementation handoff; return any genuinely invalid
> accepted input to its smallest owner.

Native model, effort and fresh history belong in dispatch fields under the
[Codex adapter](../../docs/agent-harness/codex.md), not in the brief. A mismatch
between required native APIs and accepted mechanism reopens System Design;
an unavoidable observable behavior change reopens Specification. Test
techniques, fixture choices and mechanical caller repairs remain Implementation.

Planning changed only `plan.md`, `planning-review.md`, and this transition.
No product source, test code, configuration, schema, infrastructure or upstream
phase artifact changed. No Rust build/product test, commit, push, PR, merge or
deployment ran. Static documentation checking is distinct from product proof.

Final static check: `/opt/homebrew/bin/rtk proxy make docs-check
CARGO=/Users/daniil/.cargo/bin/cargo` completed with exit 0 over all three
Planning artifacts and existing documentation: 1318 links, zero errors.
