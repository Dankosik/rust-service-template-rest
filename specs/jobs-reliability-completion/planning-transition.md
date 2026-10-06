# Planning Transition

```text
status: ready
owner: Planning
result: specs/jobs-reliability-completion/tasks.md and tasks/T1-attempt-custody.md, tasks/T2-executable-reference.md
review: specs/jobs-reliability-completion/planning-review.md — PASS
movement_evidence: both independently acceptable outcomes have closed inputs, writable owners, consuming-action dependencies and final observables; R1–R5 and HeaderValue prose are reconciled; independent readiness review found no gap
reopen_owner: none
next_owner: Implementation
```

Consume the [ready ledger](tasks.md) and its two packets, supported by the
[accepted design](design/system.md), [ownership](design/ownership.md) and
[Planning Review](planning-review.md). No implementation has begun.

Checkout:
`/Users/daniil/.codex/worktrees/jobs-reliability-followup/rust-service-template-rest`.
Branch: `codex/jobs-reliability-followup-20261005`; draft PR #240.
Planning input HEAD: `c8cf7a0af11930988fc47c3751585103abf2b047`.
Runtime/rehearsal baseline: `ac88395be87cba3a1e0587f533dc50a71e358c8d`.
The reviewed additions are local documentation; their identities are retained
in the review. Root remains continuation coordinator and may bind the existing
native carrier as `LEDGER_ORCHESTRATOR`, becoming sole ledger writer.

## Next action and proof boundary

Load [Implementation](../../docs/spec-first-workflow/phases/implementation.md)
and dispatch T1 and the independent part of T2 within their disjoint scopes.
T1's runtime fix is independently usable. T2's CLI, receiver, schema, process
driver, source carrier and guides remain one executable outcome; internal
lanes are available without inventing more acceptance units. T1's landed
code supplies the final gauge/patch binding. Test execution and review are
not schedulable tasks and do not delay the ready coding frontier.

After both tasks are Implemented and writers have joined, assign one delivery
owner the ledger's final-validation boundary. Stabilize and commit the candidate
before the real initialized-service exercise consumes an exact immutable SHA;
retain truthful scoped proof if only receipt prose changes afterward, and get
actual selected CI results for the published head. All five requirements,
bounded real measurements and independent assembled review remain pending.

Planning proof is static consistency and the independent written readiness
walkthrough; no Rust build/test, runtime, real-database, load, crash/restore or
upgrade evidence was run or claimed. Documentation link/fragment and whitespace
checks passed before handoff. No implementation code/tests or upstream accepted
design changed in this phase.

Authority carries forward: scoped edits, bounded local/CI proof, commit/push and
PR create/update. No merge, production effect, purchase or new cloud resource.
Preserve unrelated edits and use the existing carrier/validation lock. No
user-owned technical decision remains open.

Reopen Planning for an invalid task boundary/dependency; Technical Design for
a mechanism, ownership, reference API or measured-budget contradiction;
Specification for changed finality/replay behavior; Intake only for changed
requester outcome or authority. A missing required final runtime result remains
unverified; it does not prevent independent implementation from closed inputs.
