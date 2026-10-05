# Definition transition

```text
status: ready
owner: Definition (Intake and Specification)
result: specs/process-lifecycle/intent.md and specs/process-lifecycle/spec.md
review: specs/process-lifecycle/definition-review.md (PASS)
movement_evidence: All requester-meaning items have dispositions; source-supported behavior, failure precedence and budget expectations are fixed; no user-owned decision remains; independent Specification Review passed.
reopen_owner: none
next_owner: Technical Design (System / Integration Design, then Rust Code / Ownership Design where placement is not forced)
```

Continue from [Intent](intent.md), [Specification](spec.md), and the
[independent review](definition-review.md) in the same worktree. The selected
workflow revision is base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`;
[AGENTS.md](../../AGENTS.md), the [workflow router](../../docs/spec-first-workflow.md),
and [Codex harness](../../docs/agent-harness/codex.md) govern continuation.

Design must resolve cancellation-safe resource ownership, completion/failure
observation, budget composition, panic cleanup, and the smallest usable
component integration path using existing owners and dependencies. It must
justify any interface from current consumers; this phase has not accepted a
new registry, supervisor, or public service registration API. Concrete test
cases and commands remain Implementation work.

Only Definition artifacts changed. No code/build/runtime proof, commit, push,
PR creation, merge, deployment, or infrastructure change occurred in this
phase. Local edits/validation and the eventual separate PR remain authorized;
publication is the delivery owner's responsibility. Reopen Specification if a
mechanism requires changed observable behavior; reopen Intake only for changed
requester meaning or authority.
