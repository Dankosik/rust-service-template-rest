# Definition Transition Result V1

```text
status: ready
owner: Definition
result: specs/jobs-worker-reliability/spec.md;
  requester meaning in specs/jobs-worker-reliability/intent.md
review: specs/jobs-worker-reliability/specification-review.md (PASS)
movement_evidence: every audit point has a grounded behavior disposition;
  the fixed Specification passed fresh independent review with no findings;
  no unresolved user-owned input prevents the next owner acting
reopen_owner: none
next_owner: Technical Design (System / Integration Design,
  then Rust Code / Ownership Design where placement is not mechanically forced)
```

## Continuation boundary

The root coordinator `/root` continues the accepted full fix and one separate
PR request in a fresh Technical Design actor. Definition stops here; no
production code, build, runtime verification, commit, push, or PR was performed
by this phase. The four files in this task directory are its entire writable
result. Branch: `codex/jobs-worker-reliability-20261002`; source baseline:
`67be869acea112af271ec8ba621cbc50ae9d36b7`.

Ready artifact identity: `intent.md` SHA256
`e7fc1ede41358ab464c25004fa298fd1256f2e31c646de0a9f11229cc9d59253`;
`spec.md` SHA256
`dff3e736c20e1b03e7bb9a21116c9f097b126b8fa401b096402fac678b0c9943`.
The review records the preceding fixed hash and the sole status-only refresh.

Behavior is fixed by [spec.md](spec.md). Design must close the bounded full
attempt lifetime and cancellation bookkeeping, PostgreSQL-only operator
inspection/recovery commands, recoverable version and history representation,
unique-key concurrency and unknown commit outcomes, failed observation, and
append-only/mixed-version rollout needs. Use existing crate/library mechanisms
and owners; do not reintroduce a framework replacement or an unrequested SLO.

Existing user authority covers scoped changes, validation, commits, push, and
one separate PR. It does not cover merge, deployment, or commands against a live
queue. Reopen Intake only for changed requester meaning or effect authority,
Specification for changed behavior/retention/effect identity, and the relevant
technical owner for a mechanism gap that preserves this contract.
