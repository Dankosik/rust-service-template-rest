# Consumer lifecycle technical design

Status: ready. Technical Design owns this decision set; [Specification](../spec.md)
owns behavior and [Intent](../intent.md) owns scope. This design is based on
`2cb871895b9edd018205fc98223477e269fce2e9` in the consumer-lifecycle worktree.
The [Definition transition](../definition-transition.md) remains authoritative
for its earlier source boundary and explicit PR250 dependency. The coordinator
owns final dependency admission before dependent Implementation.

## Selected mechanisms

| Behavior | Mechanism and enforcing owner | Accepted cost and alternative disposition | Reopen condition |
| --- | --- | --- | --- |
| U1–U4 | Full historical public initialization, retained rendered Git commits, native explicit-base three-way merge, separate preparation and acceptance; [upgrade design](runtime-upgrades.md) | A narrow Python-stdlib/Git lifecycle command owns admission and custody. Existing generation semantics remain authoritative. Copier would need a second rendering/answers bridge and cannot reconstruct missing historical inputs; cargo-generate does not supply the required update lifecycle. A plain copy, two-way diff, or cheap projection cannot identify consumer edits. | A maintained tool can use these exact historical generation rules and custody semantics with lower total maintenance, or a supported Git version cannot represent required conflicts. |
| R1–R3 | Two real consumer commits and distinct registry digests through the existing native CD action; digest-addressed local disposable deployment and observed rollback; [release and recovery design](release-recovery.md) | One durable-profile consumer traverses the entire cycle; a separate minimal local consumer exercises initialization/build. No hosted deployment platform or paid runtime is needed. The extra version is required by real rollback. | Target plan cannot support the retained attestation path, the runtime cannot run the published architecture, or the selected pair changes schema/handlers incompatibly. |
| D1–D4 | Existing integration/Compose harnesses, two historical adapter builds, native PostgreSQL logical dump and NATS V2 snapshot/restore, fenced recovery, durable logical-ID reconciliation | Extend existing proof fixtures and add a bounded source-only rehearsal command. No backup format, runtime recovery service, production schema, provider upgrade, or generic fleet detector. Native tools meet the finite synthetic drill; pgBackRest/PITR adds an unrequested archival contract. | Changed provider/topology/backup format, incompatible historical APIs, or missing native restoration capability. Do not repair those by silently upgrading dependencies. |
| C1–C3 | Two native image lanes: source and serial derived shapes, joined by native job results and required artifact-upload outputs in `image`; [CI design](ci.md) | One extra runner allocation and duplicated setup/imports may lose source-to-derived local cache reuse. Four-way sharding and four new remote caches add unproved work/storage costs; retain the source cache exporter only. | Comparable native runs show no shorter selected-gate critical path, loss of reuse dominates, or the accepted quota/cost bound is exceeded. |

The custom part is repository-specific admission, Git-baseline reachability and
receipt custody, not a replacement merge engine, renderer, Cargo resolver,
backup tool, image builder, or signing implementation. It uses installed Git,
Python standard library, the current public initializer, native provider tools,
BuildKit, and existing CI actions. No Rust crate, third-party Python package,
toolchain or provider upgrade is selected.

## Truth and material flows

| Trigger and transfer | Canonical truth and finality | Failure, retry and recovery owner |
| --- | --- | --- |
| Maintainer supplies trusted source plus initialization inputs to renderer | Source commit **and its own** helper/inventory/toolchain/lock produce a full rendered tree. The immutable tree is retained in Git, with the render recipe and hashes. | Missing source/tool/input or generation failure yields no baseline. Updater reports the precise missing input; it never substitutes the current helper or cheap projector. |
| Maintainer asks for upgrade of committed consumer C | Explicit base B0, ours C and target full render B1 produce isolated candidate R. Local changes in the original checkout are untouched. | Native conflicts remain visible. A partially prepared attempt cannot change accepted metadata. Abort retains C and any independent dirty/untracked/ignored work. |
| Reviewer and validator admit R | The explicit acceptance operation seals R's resolved content plus metadata and makes B1 reachable as a Git parent. Normal clean Git adoption moves the consumer branch. | Stale C/R/evidence or remaining conflicts refuse. Content edits invalidate prior validation/review. Repeating an accepted target is a no-op. |
| Consumer source enters native CI and CD | Exact source/CI identities, image ID, published digest, signature and attestations remain separate. Existing action verifies registry content before promoting tags. | Failed/cancelled/missing gates stop publication or promotion. A run-scoped pushed candidate remains unreleased. Retry first reconciles run/tag/digest state. |
| Operator replaces A with B, then B with A | Compatibility admission, verified digests, actual started revision and durable operation identify each observed state. | Image rollback does not reverse persistent state. Failed readiness/drain leaves the current digest and incomplete stage recorded; keep stores fenced before retry. |
| Operator fences stores, archives, restores and re-admits workers | Whole synthetic database plus broker archives, identity manifest and PostgreSQL effect records establish the recovery boundary. New broker creation identities require fresh process admission. | Preserve originals; a partial restore remains fenced. Reconcile before replay, re-inspect jobs, invalidate pre-restore tokens and use another empty destination for a failed broker restore. |
| CI selection launches source and derived proof | The existing classifier and graph selector define required work; existing fail-fast runners execute it; native job results and immutable artifact-upload outputs bind the aggregate to that workflow's candidate. | A lane's failure/cancellation/absence cannot become an unselected skip. Prior successful same-run work survives a failed-job rerun; attempt number is provenance, not invalidation. |

All operations have finite existing command/process deadlines. Implementation
chooses focused proving cases and command-level time limits within the current
owners, rather than changing runtime budgets. The bounded rehearsal uses finite
synthetic identities and reports elapsed recovery; it creates no production
RTO/RPO, allowed-loss policy or indefinitely running workload.

## Source, artifact and acceptance custody

The design distinguishes three pairs:

1. The historical runtime transition uses template
   `67be869acea112af271ec8ba621cbc50ae9d36b7` to
   `2cb871895b9edd018205fc98223477e269fce2e9`. It crosses the jobs custody
   change and demonstrates **refusal** of an old-worker rollback after custody
   activation. Both use their own source/helper/lock/toolchain; historical Rust
   `1.98.1` and current `1.99.0` are existing pinned inputs, not new upgrades.
2. The real release consumer starts from full render of `2cb8718…` and accepts
   the final admitted lifecycle template revision F through the supported
   upgrade. F is the single immutable Implementation candidate recorded after
   review/CI, not an implementation choice of a different upstream. Release A
   uses service version `0.1.0`; B uses `0.1.1`. The release pair retains the
   corrected jobs behavior and identical schema/job/event contracts. The version
   and `VCS_REF` changes ensure distinguishable artifacts; equality of registry
   digests is nevertheless checked and would fail the rollback claim.
3. The CI comparison uses identical final release build inputs under serial and
   split scheduling. The exact `2cb8718…` native run supplied by the coordinator
   is a reference only to the extent its input/cache fingerprints match. The
   older `c3a3f18…` timing is historical context, not a matched control for the
   newer dependency graph.

Before release A/B preparation, compare F with `2cb8718…` for migration, handler,
wire, configuration and lifecycle changes. This task is expected to change
support/proof/CI only. An unexpected incompatible runtime delta reopens this
design; it cannot be waved through by the version labels. Consumer commit IDs,
baseline tree IDs, fixture overlay hashes, exact CI runs, image IDs and registry
digests are measured outputs and are filled when produced, never invented in
Design. Their selecting rules and admission owners are fixed here.

## Scope and phase boundary

[Ownership](ownership.md) fixes source/proof placement. No production Rust
implementation or database schema change is designed. Synthetic fixture state
lives in disposable test databases; its actor is not shipped in template or
consumer images. A consumer may keep its own small source changes to demonstrate
preservation; they are service-owned and recorded separately from B0/B1.

The coordinator already holds authority for template fixes through PRs/push and
normal native CI. The proposed consumer remote identity, visibility, registry
writes/tags and retention envelope in [the operation preparation](release-recovery.md#proposed-external-envelope)
still require matching authority. Finish local generated-source and artifact
preparation before asking for those consequences. This restriction does not
stop technical implementation or local synthetic proof.

Technical Design completion is a fixed decision set, static consistency/link
proof and fresh Technical Design Review. It does not claim an implemented
updater, generated consumer, successful restore, faster native run, published
digest or observed deployment. Planning remains the next fresh phase; it must
retain actual release/recovery/CI observations as open execution outcomes.
