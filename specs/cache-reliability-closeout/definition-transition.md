# Definition transition

```text
status: ready
owner: Definition
result: specs/cache-reliability-closeout/spec.md
review: specs/cache-reliability-closeout/definition-review.md (PASS)
movement_evidence: requester meaning closed; source-grounded behavior covers all justified findings; fresh independent Specification review permits movement
reopen_owner: none
next_owner: Technical Design (System / Integration Design, then Rust Code / Ownership Design if placement is not forced)
```

Authoritative inputs: [Intent](intent.md), [Research](research.md),
[Specification](spec.md). Baseline: `67be869acea112af271ec8ba621cbc50ae9d36b7`,
branch `codex/cache-reliability-closeout-20261002`, worktree
`/Users/daniil/.codex/worktrees/cache-reliability-closeout/rust-service-template-rest`.
Workflow authority is this checkout's `docs/spec-first-workflow.md`, shared
Review/Transition and `docs/agent-harness/codex.md` at that baseline.

Technical Design selects the smallest supported mechanism for stalled-command
recovery, owned cancellation, rejected password-rotation recovery and safe
dependency diagnostics. It derives finite recovery/resource bounds from the
existing budgets, resolves their interaction, and records placement consistent
with feature/provider dependencies. No new user decision is missing.

The one bounded assumption is absence of an existing feature adopter to
migrate; reopen Specification if an adopter changes compatibility. Changed
driver behavior reopens Research. Inability to meet bounded recovery/cleanup
under current budgets returns to the responsible design/specification owner,
not to a user choice of implementation mechanism.

Definition changed only this task's artifacts. `make docs-check` passed for the
fixed specification (849 links, zero errors); `git diff --check` passed. No
production code, heavy validation, runtime, CI, merge or deployment result is
claimed. Implementation and PR delivery remain with the root's continuing
authorized outcome.
