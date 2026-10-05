# Definition transition

```text
status: ready
owner: Definition
result: specs/tokio-progress/spec.md
review: specs/tokio-progress/definition-review.md (PASS)
movement_evidence: requester scope and behavior closed; fresh independent Specification review passed; no missing user-owned decision
reopen_owner: none
next_owner: Technical Design — System / Integration Design, then Rust Code / Ownership Design where needed
```

Authoritative inputs are [Intent](intent.md), [Specification](spec.md),
[baseline](research/baseline.md), and [independent review](definition-review.md).
The branch is `codex/tokio-progress-20261005`, based on
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, in
`/Users/daniil/.codex/worktrees/tokio-progress/rust-service-template-rest`.

The accepted delta is bounded upload polling, nonblocking bounded best-effort
logging with explicit loss and final-flush outcomes for all current consumers,
and corrected guidance for actual blocking execution lifetime. CPU executors,
thread tuning, blanket offload and new infrastructure remain excluded. The
library, capacities, signal names and ownership layout remain Design decisions.
No runtime behavior has changed and no production/performance claim is made.

The Definition owner ran `make docs-check` successfully (1,264 references,
zero errors before adding these receipts) and `git diff --check` clean. Final
receipt-link validation is included in the handoff result. Existing execution
and CI requirements remain with their owners; this phase adds no proof gate.

Continue with a fresh Technical Design actor. Compare maintained writer options,
resolve the existing-budget lifecycle through terminal records and all binary
consumers, and select the smallest upload/guidance changes. Reopen Definition
only for changed requester meaning or behavior; no implementation is authorized
to this phase actor. The parent retains scoped implementation/validation/push/PR
authority, with no merge or deployment.
