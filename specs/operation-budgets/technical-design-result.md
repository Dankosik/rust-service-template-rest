# Technical Design result: operation budgets

```text
status: ready
owner: Technical Design — System / Integration Design and Rust Code / Ownership Design
result: specs/operation-budgets/design/system.md and design/ownership.md
review: specs/operation-budgets/design/technical-review.md — PASS, no surviving findings
movement_evidence: mechanism and exact ownership closed; all three ownership lenses PASS; Technical Design Review PASS after one bounded finality repair
reopen_owner: none
next_owner: Planning
```

Canonical inputs: [reviewed Definition](definition-result.md),
[specification](spec.md), [intent](intent.md), and
[baseline evidence](research/baseline.md). Candidate D1-r1 uses baseline
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, branch
`codex/operation-budgets-20261005`, worktree
`/Users/daniil/.codex/worktrees/operation-budgets/rust-service-template-rest`.
Current phase/workflow owners were read from that checkout. Only this phase's
`design/*` and this receipt were written; no code/config/dependency changes.

| Ready artifact | SHA256 |
| --- | --- |
| [System design](design/system.md) | `1a26e99c5aaf0ae1f25bb85ccf0bdd54536f14e4cf4edb0fbdd665726e8e2a62` |
| [Ownership map](design/ownership.md) | `23e23268b21510fd5ffbe648ca04d1197de0badb6c0b5e72632a229fb5c88bed` |
| Accepted spec.md | `02c087f230e579c3bf3eecd20237692e9fe5038293befd29ef578993db330de1` |

The [Technical review receipt](design/technical-review.md) records pre-promotion
hashes and the unchanged-semantic status promotion. The
[ownership receipt](design/ownership-review.md) records the complementary lenses.
No unresolved reviewer concern remains.

## Decisions carried to Planning

- Add one always-retained neutral `operation-context` leaf using existing Tokio
  time/cancellation and the existing overflow-safe gRPC Deadline representation.
  Exact provider/transport edges and profile/architecture custody are fixed in
  the map; no new registry package, upgrade, retry framework, duration or reserve.
- HTTP and gRPC opening expose the same fixed cutoff before auth/business work.
  Distinct explicit response contexts preserve uncapped generic streams and
  caller-supplied longer gRPC lifetimes. Successful headers transfer cancellation
  custody; they do not cancel an S3 body transferred into the response.
- Auth/cache/S3/outbound use one parent-aware enforcement path plus their current
  standalone finite ceiling. Moka native takeover stays its owner; no new
  shared-future registry or exactly-one-physical-exchange guarantee.
- OAuth prepares an opaque call bound to its concrete gRPC client before any
  credential path. FullRpc and OpeningOnly retain their selected interval,
  parent/caller lineage and original entry through token work and body ownership.
- S3 keeps SDK retry ownership and single-attempt mutation policy. A local weak
  expiry task removes SDK body/withheld chunk/permit from the Download resource
  cell at D/cancellation even without polling; no producer or queue is added.
- Confirmed mutation results from a live-started final synchronous SDK poll
  retain the current adapter result. Pending stop remains OutcomeUnknown; the
  outer terminal owner enforces its original cutoff. No new storage error or
  retry eligibility is introduced. Complete GET/bytes success still requires
  confirmed EOF within D.

## Proof, next action and reopen boundary

Planning may form the smallest dependency-ordered implementation units from the
ownership map without selecting a new carrier, flow, result API or lifetime
owner. The executor chooses focused tests under final-validation ownership;
required questions are pre-dispatch refusal, shared-wait isolation, selected
OAuth interval, body EOF, no-poll resource release and uncertainty without replay.
Several crates/manifests are affected: build/test and routed validation apply,
with the existing profile/integration/CI owners retained. Lockfile changes must
be deliberate and preserve current registry resolution. No validation command
was run in this Design phase; current proof is source/design review only.

Reference #242's path-limited exact-cutoff mechanism is reusable; #248 exact
head did not contain the claimed no-poll fix and is not proof for it. No unrelated
PR patch, vendor change or worktree was imported. PostgreSQL budgets/reserve,
CommitUnknown/native return custody, attempt/backoff/settlement owners and
production configuration remain outside this implementation delta.

Reopen System Design for a mechanism contradiction, affected ownership rows for
placement evidence, and Specification for a real behavior/authority conflict.
The explicit conditions are documented at the end of system.md and per map row.
No requester-owned input is missing. Continue through the root coordinator;
commit/push/one separate PR remain authorized downstream, merge/deploy/new
infrastructure are not part of this phase or the accepted delivery boundary.
