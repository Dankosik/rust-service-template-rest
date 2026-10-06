# Consumer lifecycle

status: ready

Ledger Orchestrator: `/root` is bound as `LEDGER_ORCHESTRATOR` and is the sole
writer of this index. Native unit Leads own implementation under their packets.

Completion: The assembled template supports U1–U4 consumer-preserving runtime
upgrades; actual minimal and durable consumers have been prepared and validated;
corrected consumer releases A and B have native signatures, provenance, SBOM and
registry-digest verification followed by observed A→B→A operation; the separate
historical jobs-custody and native PostgreSQL/JetStream recovery rehearsal has
executed successfully; and comparable native serial/split evidence establishes
a shorter selected-gate critical path while preserving source and shapes
`1,7,47,65`. All required local/CI evidence and one independent integrated
delivery review pass on their actual candidates. Each missing requested result
remains outstanding; a guide, Implemented checkbox or local pass is insufficient.

Global constraints: [Specification](spec.md), [Technical Design](design/system.md),
[ownership](design/ownership.md), [release/recovery gates](design/release-recovery.md#ordered-operational-gates),
and [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md)
govern execution. Tests are written with code and executed at the single final
validation boundary after all four units are Implemented and assembled. No
per-task review, proof task, persisted wave or partial-final-validation bypass.
Only the bound Ledger Orchestrator writes this index during Implementation.
Overlapping writers take the packet locks serially; refill the frontier after
each integrated Implemented result. Refresh disk/Docker/native-tool capability
only before the consuming heavy generation or final execution; the reported
capability gap does not prevent independent code work.

## Tasks

- [x] T1: A supported full-render/native-Git runtime upgrade preserves consumer work and advances only an explicitly accepted baseline.
  - Depends on: admitted PR250 source `2cb871895b9edd018205fc98223477e269fce2e9` and ready Design; available now.
  - Provides: updater commands, baseline custody, user route and authored focused proof.
  - Packet: [T1](tasks/T1-runtime-upgrades.md)
  - Result: Implemented; uncommitted bounded-file aggregate SHA-256 `87dd1d3abf4c0c7c3dad0f4d567dda7b586b75be0fc7ac6c038ddb54addef4b9`. Full suite, real rendering and assembled acceptance remain pending Completion; earlier native scenario is only its stated coding-feedback scope.
- [x] T2: One source-only native lifecycle rehearsal can execute the fixed historical custody transition and fenced durable-state recovery.
  - Depends on: admitted PR250 source and ready recovery/ownership contracts; available now. Native local runtime/tool capacity gates its actual execution at Completion, not coding.
  - Provides: bounded rehearsal carrier, historical actor/fixture sources, routing and operating guide.
  - Packet: [T2](tasks/T2-native-recovery.md)
  - Result: Implemented; uncommitted source-only actor, ignored integration scenario, native carrier, guide and linked-owner blocks in the shared candidate. Historical compilation and actual native rehearsal remain unexecuted; writers stopped and shared routing integrated by T1.
- [x] T3: Native source/derived image lanes retain complete admission and comparable timing/cost evidence.
  - Depends on: admitted PR250 source and ready CI contract; available now. Native exact-candidate CI and matched comparison gate Completion, not coding.
  - Provides: two-lane workflow, stable aggregate and identity/timing evidence through existing owners.
  - Packet: [T3](tasks/T3-image-ci.md)
  - Result: Implemented; bounded uncommitted workflow/measure/recorder/native-results-helper diff in the shared candidate. Verification and C2 measurement remain pending Completion; T1/T2 own the serialized source inventory/routing closure.
- [x] T4: Actual isolated minimal and durable consumer repositories contain the reviewable A/B source preparation and concrete publication proposal.
  - Depends on: integrated Implemented T1, T2 and T3 form frozen template source F. Local generation capacity gates generation itself. Consumer validation, upgrade sealing and final A/B admission gate Completion. Missing consumer repository/GHCR effect authority gates only the named publication/settings/ref actions after preparation.
  - Provides: real generated Git repositories, retained consumer edits, full target render and B source-preparation inputs, exact source/resource inventory and preparation receipt.
  - Packet: [T4](tasks/T4-consumer-preparation.md)
  - Result: Implemented; actual minimal F `32f469707cdf1514fece814b23af82e6fa18b788`, durable pristine A `384815a674ea9e8f08f8f2e3d6b43a968f249ccd`, consumer A `69c0385e2be74333b8b04fd720a2efd9adb61b24`, B source evolution `739a1ffb27935fb1ccb81d628b2aab9e86051c6b`, and pristine F `17703400b5519195e169b204ae6646ac5befd80c`. [Preparation](consumer-preparation.md) records exact paths/inputs; supported upgrade, seals and validation remain Completion. Writers stopped.

## Completion custody

After all four code/source units are assembled and writers have joined, assign
one delivery owner final validation, review, authorized delivery and evidence
collection. Freeze each consumed candidate; repairs return to its existing unit
and invalidate only affected proof. Tests, native historical recovery, native CI
comparison and release observations are Completion work, never extra ledger rows.

The delivery owner first validates the assembled template and prepared consumers
under their repository owners. In this single Completion stage, establish A
initial capture/adoption through its required content validation and maintainer
review; only then run the supported updater to prepare B from the already
authored consumer evolution. Validate resolved B and obtain the one independent
integrated review covering updater, restore custody, CI admission and consumer
seams. Bind reviewed content and baseline before the metadata-only B acceptance
seal, then freeze final A/B commits for native CI/publication. These ordered
operations are the accepted upgrade/release exercise inside Completion, not
new scheduler tasks or independent per-task review gates. Record source F, A/B content and sealing
identities separately. A repair that changes runtime content requires affected
proof before acceptance; do not relabel a pre-seal result as a final-commit run.

Template PR/push and normal native CI are already authorized. Consumer repository
creation, visibility/settings, refs and GHCR writes remain gated by the root's
receipt of the missing [external envelope](design/release-recovery.md#proposed-external-envelope)
authority after concrete local preparation. Its proposed zero cash ceiling,
bounded cycles and 30-day retention are not silently granted. Execute remaining
authorized local final proof while that effect gate is pending. No paid host,
new runtime installation, public visibility fallback or shared-cache clearing.

Keep successful corrected publication rollback (`2cb` A→F B→A) distinct from
the historical negative transition (`67be869`→`2cb`). Never renumber applied
migrations, treat a successful old pre-custody rollback as custody-safe, or
replace actual published digests with local image IDs. The completion receipt
separately reports local support, CI admission/improvement, registry trust,
run/rollback and native durable recovery, including unavailable required scope.
Mark `done` only after the whole Completion is Accepted. Stage 12 announcement,
topics, badges and listing remain with their original owner.
