# Technical Design review

```text
candidate: Five fixed design files on source 2cb871895b9edd018205fc98223477e269fce2e9; reviewed SHA-256 values below
verdict: PASS
findings: none
evidence_boundary: Fresh read-only Technical Design Review against ready Definition and current source/provider authorities; no implementation or execution claim
reopen_owner: none
```

Reviewer: fresh native `/root/consumer_design/design_review`, `reviewer-agent`,
explicit `gpt-6-astra` / `xhigh`, `fork_turns: none`. The successful native
dispatch returned that identity after the coordinator repaired a disk-capacity
failure that had prevented the earlier dispatch from creating any actor.
The reviewer owned no files, repair, acceptance or transition.

## Fixed reviewed candidate

Paths are relative to `specs/consumer-lifecycle/`; each hash was unchanged
before and after independent review.

| File | SHA-256 |
| --- | --- |
| [System](design/system.md) | `ec8985420f33da4ff67c9badecba65f8019f27be7fa43c2eade69f4ecc86a771` |
| [Runtime upgrades](design/runtime-upgrades.md) | `e6a959507fe571342adac55cc0a8b32fc5b5a0f19ded2af09ad7936600256569` |
| [Release and recovery](design/release-recovery.md) | `d1328cfef1f9dd64c2e37db0ca5085385b8474c2ec616c4f82951805eefcfd68` |
| [CI](design/ci.md) | `fbffa6dc5805dd41626581b8b958058c237cc073827e2cac56d69bb81661837d` |
| [Ownership](design/ownership.md) | `1d548ed5b2e33647cff179d068567f43ede01629712826c507a2943c900cd660` |

After PASS the phase owner changed only `Status: draft` to `Status: ready` in
these five files. This status-only promotion changes hashes without changing
mechanism, proof, interfaces or review scope; the Transition records final
hashes. No bounded repair or recheck was needed.

During Implementation's narrow containment question, the same Design owner
clarified that updater CLI/library/proof are source-only while the operational
guide remains in generated consumers, and named retained `image-results.py` as
the location of the already-selected small native CI aggregate. The current
ownership/runtime/CI files record that clarification; the Transition carries
their refreshed hashes. This is a placement and link-closure refinement of the
trusted-template invocation and native aggregate already reviewed here. It
changes no command contract, truth/acceptance owner, runtime boundary, proof
scope, portable-sync ownership or retry semantics. The original semantic review
remains applicable under Transition's unchanged-scope rule. Static delta review
belongs to the Design owner; no new independent verdict on the refreshed bytes
or per-task review is claimed.

## Attempted falsifiers and result

- **Baseline custody advances without validated content.** Full historical
  public initialization, reachable baseline Git parents, explicit-base merge,
  conflict disposition, content-bound evidence and metadata-only sealing close
  the path. The original dirty checkout remains outside the isolated operation.
  Native [Git merge-tree](https://git-scm.com/docs/git-merge-tree/2.49.0) supports
  the chosen merge/conflict interface.
- **Legacy adoption silently calls an evolved tree pristine.** Captured and
  reconstructed baseline evidence are distinguished. Adoption requires
  unambiguous historical inputs and review of the complete consumer difference;
  unexplained evidence refuses. Cheap-projector byte equivalence is not assumed.
- **Additive DDL implies universal old-binary admission.** The imported-migration
  gate preserves the matching-prefix boundary. Independent inspection of
  `crates/migrate/src/lib.rs` confirmed refusal of unknown applied versions
  inside the embedded range. The historical pair has exactly the two stated
  SQL additions and no changed existing migration bytes.
- **Custody transition is mislabeled as safe rollback.** The pre-custody
  `67be869…` to corrected `2cb8718…` transition is separate from corrected
  consumer A/B. Every old retention owner stops before custody activation;
  rollback to it then refuses, agreeing with the current jobs owner.
- **Fixtures replace the real mechanism.** Historical/current actors remain in
  the existing integration package, preserve pristine source identities and
  cannot patch production code. Published-worker observations are distinct from
  fixture consumption. Existing dependencies, stable APIs and durable logical-ID
  effect precedent support placement. The fenced native sequence respects
  [PostgreSQL restore](https://www.postgresql.org/docs/18/app-pgrestore.html) and
  [NATS V2](https://github.com/nats-io/nats-architecture-and-design/blob/main/adr/ADR-63.md)
  restoration limits.
- **Green CI accepts missing work or rejects valid retry reuse.** Selection
  reaches existing fail-fast command owners. Selected native jobs and required
  immutable upload outputs must succeed. Producing attempt remains provenance;
  native successful prerequisites may survive a failed-job rerun at the same
  SHA/ref. No redundant receipt-verification engine is required.
- **Parallelism is claimed as measured improvement.** Matched serial/split native
  observations, cache conditions, complete selected proof and total runner/cache
  cost are required. Unmatched/inconclusive results leave C2 incomplete.
- **Missing consumer authority blocks authorized template work.** Template
  PR/push/normal-CI authority is retained. Proposed consumer repository,
  visibility, registry refs, quota and retention remain separately reviewable
  consequences, sought only after local preparation.

## Proof limit

No edits, builds, runtime probes, provider experiments or remote writes were
performed by the reviewer. Actual rendering, historical actor compilation,
restore, publication, observed rollback and measured CI improvement remain
Implementation obligations. Docker availability and adequate disk capacity
must be refreshed before execution; they do not invalidate the reviewed
mechanism or justify provider/runner upgrades.
