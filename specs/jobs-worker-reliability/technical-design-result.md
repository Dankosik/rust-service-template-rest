# Technical Design Transition Result V1

```text
status: ready
owner: Technical Design
result: specs/jobs-worker-reliability/design/system.md;
  specs/jobs-worker-reliability/design/ownership.md;
  specs/jobs-worker-reliability/research/mechanisms.md;
  specs/jobs-worker-reliability/rollout.md
review: specs/jobs-worker-reliability/technical-design-review.md (PASS);
  design/ownership-review.md records all three ownership lenses PASS
movement_evidence: B1-B5 mechanisms, resource/cancellation budgets, CLI/config
  projection, storage/history/version semantics, unique-key arbitration,
  unknown finality, sampling/discovery, exact Rust ownership and additive
  mixed-worker rollout are closed; no surviving review finding or required
  external input prevents Planning
reopen_owner: none
next_owner: Planning
```

## Fixed ready input locators

All paths are under `specs/jobs-worker-reliability/` in worktree
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-jobs-worker-reliability-20261002`,
branch `codex/jobs-worker-reliability-20261002`. Source baseline is
`67be869acea112af271ec8ba621cbc50ae9d36b7`.

| Artifact | SHA256 |
| --- | --- |
| [Specification](spec.md), unchanged ready behavior | `dff3e736c20e1b03e7bb9a21116c9f097b126b8fa401b096402fac678b0c9943` |
| [System design](design/system.md) | `14a085cc48eced41e134c32d329af702be5884cc29ed0e1e85dbe94e16449e54` |
| [Ownership map](design/ownership.md) | `b4c69439173d668e104862fc9abc3b8dcd75dfcc0069d7394554554f542ad9fb` |
| [Mechanism evidence](research/mechanisms.md) | `8a5d74f6ca5a4e0a8f2214638199439939a1266fecb9d3922d020e093610abf0` |
| [Rollout](rollout.md) | `f018247d09062a94447897cd124a9d0df060712194f8a279e3e9e37ad4f7e70d` |
| [Ownership panel](design/ownership-review.md) | `513f383e849e543d6d8d7c36e53747374e00e8a68bbe5866e6a8e72d80c1a6c8` |
| [Technical Design Review](technical-design-review.md) | `3e7531a0641aba3a16fe3c1371a0237038006890d8eed8741560e126b940d78a` |

The review identifies its fixed pre-status-refresh hashes. After PASS only the
status line of system, ownership and rollout changed from draft to ready;
the reviewed semantics are identical. The research and panel hashes are
unchanged. The root coordinator re-reads these authorities before dispatching a
fresh Planning actor; this phase actor does not enter Planning or Implementation.

## Material decisions carried forward

- Keep one admission object through supervisor and queued/in-flight completion
  cleanup; synchronous unregister, batch deadlines and retirement-before-reply
  prevent growth across permit reuse. Keep existing grouping and lifecycle.
- Retain failed rows. Reuse the current claim-generation sequence as the
  recovery fence; archive the prior cycle in an additive history column and
  reset the same row atomically through the shared Tx. The live-key index owns
  conflicts, and unknown commit never becomes success or automatic replay.
- Jobs-worker gains one-shot inspection/redrive/discard before ordinary
  bootstrap. Only the PostgreSQL config projection is admitted; no handlers,
  broker, listeners or unrelated secrets are required. Existing budget
  ceilings give the stated 33-second network/teardown maximum.
- Page by bounded primary-key scan with explicit fleet kinds, preserving empty
  partial continuation. Add capped failed depth and move the union sample to
  the existing process-duty owner to keep ordinary/publisher freshness coherent.
- Add history then a concurrent failed-kind index through forward migrations;
  preserve old-writer schema compatibility, but require every old retention
  owner stopped before asserting custody/recovery activation. Static leases,
  one publication slot and combined worker failure domain remain accepted.

## Proof, authority and stop boundary

Evidence here is static source/API/design review only. A read-only check found
all task-artifact relative file links resolve; it is not a substitute for the
repository's final docs-check. No production file, dependency declaration,
migration or generated output was changed; no build, runtime test, commit,
push, PR, deployment or live-queue operation was performed in this phase.
All four review descendants completed and released their read-only scopes.

Implementation selects tests under its existing final-validation owner and
uses current build/test, real PostgreSQL, SQLx, migration and selected
initializer/CI paths. The design creates no second test environment, lost-ack
proxy, benchmark SLO or repeated full-build matrix. Required final assembled
delivery review remains in scope. Distinguish local proof, PR/CI receipts and
runtime claims at delivery.

Existing human authority permits the complete coherent correction, scoped
validation/commit/push and one separate PR. It does not permit merge, deployment
or running inspection/recovery/discard against a live queue. No human-owned
decision is outstanding. Reopen Technical Design for a concrete mechanism or
ownership contradiction, Specification for a behavior/retention/identity delta,
and Intake only for changed requester meaning or external-effect authority.
