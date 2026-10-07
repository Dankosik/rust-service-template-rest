# Planning transition

```text
status: ready
owner: Planning
result: specs/consumer-lifecycle/tasks.md
review: specs/consumer-lifecycle/planning-review.md — PASS
movement_evidence: Four independently acceptable units cover every accepted obligation; implementation inputs and mutable owners are closed; later capability/authority gates are placed at consuming actions; fresh Task Review found no findings
reopen_owner: none
next_owner: Implementation — root binds the sole Ledger Orchestrator and dispatches Acceptance-Unit Leads
```

## Authoritative result

[Tasks](tasks.md) owns dependency order, implementation status/results and global
Completion. Its four [packets](tasks/T1-runtime-upgrades.md) retain individual
outcomes and final observables; the index links each packet. Accepted
[Definition](definition-transition.md) and
[Technical Design](technical-design-transition.md) remain unchanged.
The [fresh review](planning-review.md) owns the Planning verdict and falsifiers.

Worktree:
`/Users/daniil/.codex/worktrees/consumer-lifecycle/rust-service-template-rest`;
branch `codex/consumer-lifecycle-20261006`; source
`2cb871895b9edd018205fc98223477e269fce2e9`. This phase wrote only `tasks.md`,
four `tasks/*.md` packets and its review/transition receipts. All are local
uncommitted task artifacts. Runtime, CI, accepted spec/design and original
source remain unchanged.

Final ledger SHA-256 after its status-only ready promotion:
`09c4d2b969bce543247c94d78ad2168636100b095db0ad93242c6956db8a7d52`.
The four packet hashes remain the reviewed values in the review receipt.
Review receipt SHA-256:
`e81adbf8be3876d867fe81953800f52f634a9c1811b35db4c11b039ffa10e475`.

## Obligation disposition and ready frontier

| Accepted obligation | Implementation disposition | Final proof location |
| --- | --- | --- |
| U1–U4 supported full rendered runtime upgrades, preservation, adoption and custody | T1, including docs, authored proof and owned routing | Completion: focused upgrade claims plus actual T4 consumer use |
| D1–D4 historical custody transition and native durable recovery | T2, including historical actors, finite source-only carrier, routing and guide | Completion: actual one-tuple native rehearsal and immutable durable evidence |
| C1–C3 factored source/derived scheduling and full admission | T3, including native aggregate, identity/timing and owner parity | Completion: exact CI plus matched serial/split time/cost evidence; improvement cannot be inferred from green CI |
| R1–R3 actual minimal/durable consumer, published trust, run and rollback | T4 actual local source preparation; existing native release path needs no replacement | Completion: consumer validation, supported adoption/upgrade/seal, final native publication, registry trust and distinct-digest A→B→A |
| PR250 source/generated admission and shapes `1,7,47,65` | Already admitted dependency; retain through T1–T3 deltas, no duplicate implementation | Current dependency receipt has its original scope; final changed-candidate CI remains required |
| Portable sync and one-time profiles | No implementation-changing obligation; preserve owners through T1/T4 boundaries | Changed-surface proof observes preservation; no profile migration is promised |
| Production RTO/RPO, data-loss policy, topology | No implementation in this synthetic outcome; remain service-owned | Rehearsal cannot establish production policy |
| Remaining Stage 12 announcement/topics/badges/listing | Scope remains with original stage owner | Consumer Completion does not close the whole stage |

T1, T2 and T3 are ready now from closed source/contracts. Their own code,
fixture and workflow work is independent; shared initializer inventory,
self-test/classifier/make routing and common docs mutate serially under packet
locks. Assign disjoint lanes or hold only conflicting writers rather than
inventing dependencies between the independent outcomes. Integrate completed
code, release scopes and refill the frontier without waiting for checks/review.

T4 begins from their assembled Implemented template F. It creates real local
consumer source and full rendered target inputs, not an accepted upgrade without
evidence. All test execution and the actual adoption/prepare/accept exercise
remain within the single Completion stage. Completion first establishes A's
accepted baseline through its required content proof/maintainer review, then
prepares and validates B, obtains the independent integrated review, seals its
unchanged content and freezes final A/B commits before native CI/publication.
This closes the baseline-precondition cycle without creating per-task review
gates. Implementation chooses tests, fixtures and commands.

The root may bind as the sole Ledger Orchestrator under the current
[Implementation carrier](../../docs/spec-first-workflow/phases/implementation.md#carrier).
It records native Lead identities on dispatch; none is allocated in this phase.
Only the current root continues coordination. This Planning actor stops at this
boundary and leaves no active writer or descendant; its reviewer has completed.

## Current dependency admission

The coordinator's 2026-10-06 update closes the pending PR250 admission text in
the older Definition/Design handoffs for continuation purposes:

- Fresh independent integrated review: PASS, no findings at exact `2cb8718…`.
- [CI 37490935654](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37490935654)
  and [CodeQL 37490935709](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37490935709)
  succeeded. All selected jobs and required aggregates passed; gRPC and Rust
  CodeQL were intentionally unselected. PR250 remains OPEN/CLEAN; no merge.
- Image job `112363276629`: 15:51:13–16:21:33 UTC, 30m20s;
  source build 449s and derived work 1280s. These are current baseline facts,
  not a causal speed claim against the older c3 run.
- Native image proof artifact `11426727468`, downloaded by root to
  `/tmp/artifact-compatibility-main-2cb8718`: 28 log hashes verified, state
  passed, five SBOM image/revision bindings verified. Receipt SHA-256
  `8432e7c98dfef21fde77da254f0e1730eda78a24624ae01bf056d46ebe7f9294`.
  Checked-out PR merge `3df26b282b0416df2bea15fdc054f20a1bddf67d` and private
  candidate `065bae43190a4dc27cccbcb286a2c6d4be853aa8` remain separate IDs.

Planning consumes that coordinator receipt; it did not repeat external reads
or relabel the dependency proof as the future F candidate's evidence.

## Capability, effects and proof boundary

Root reported a successful 1 MiB write probe and roughly 141 MiB free after
removing only its own generated CodeGraph index in a separate worktree; the
current lifecycle index was preserved. That capacity is not suitable for heavy
generation or validation. Docker's socket is absent. Refresh adequate disk,
Docker/platform and native backup tooling before those consuming actions;
continue independent code work. No shared-cache cleanup, new runtime or paid
host is authorized merely to remove this gap.

Planning ran no builds, tests, generation, provider probes, native recovery,
CI dispatch, remote writes or publication. Its written readiness dry run and
fresh independent review passed. The existing five-design-file native docs-check
PASS remains limited to its unchanged scope. Its final two-receipt native rerun
was unavailable; all seven new Planning files likewise still require the scoped
native docs-check when existing Docker capability returns. Static link/owner
readback and valid review are not reported as that native checker passing.
The root carries this exact pending documentation check into final validation;
do not repeat the unchanged failed path or invent a substitute checker/runtime.

Template PR/push and normal native CI authority already exists. The concrete
consumer [external proposal](design/release-recovery.md#proposed-external-envelope)
still needs the root's user-owned effect decision after actual admitted A/B
commits and runtime/resource inventory are ready. Keep private repository/account
support, GHCR/settings/ref writes, zero incremental cash, bounded cycles and
30-day retention explicit. Missing consumer authority does not block independent
code or authorized local final proof; neither a guide nor local success closes
the requested publication/run/rollback/recovery/improvement result.

## Reopen and continuation

The next action is root carrier binding and ready-frontier dispatch. Narrow
mechanical locator/lock changes remain Implementation scheduling; missing tests
are executor work. Reopen Technical Design only for an invalid accepted merge,
rendering, historical API, compatibility, native recovery or measured CI strategy;
reopen Definition only for changed requester behavior/scope or demonstrated
infeasibility. The root owns capability recovery and the eventual missing effect
authority question. Preserve independent work and valid scoped proof throughout.
