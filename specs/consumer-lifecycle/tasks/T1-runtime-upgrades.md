# T1 — Consumer-preserving runtime upgrades

Outcome:
An initialized consumer can use a supported command route to adopt/recover its
complete rendered baseline, prepare a native three-way update in isolation,
resolve it and explicitly accept a validated result without losing consumer
work or silently advancing accepted custody. This replaces the absence of a
runtime-update route; portable sync remains within its existing boundary.

Consumes:
- [U1–U4](../spec.md#u1-upgrade-inputs-and-support-boundary) — behavior.
- [Runtime upgrade design](../design/runtime-upgrades.md) — selected trust,
  full historical rendering, schema-1 custody, Git ancestry, merge, acceptance
  and retry mechanism; no new mechanism decision is delegated.
- [Ownership](../design/ownership.md) — updater, shared-input reuse, user route
  and updater proof placement on admitted source `2cb8718…`.

Provides:
- Supported `adopt`, `prepare`, `status`, `accept`, `abort` commands, complete
  rendered-baseline custody and actual usage documentation.
- Authored focused updater proof and its existing initializer/source route;
  T4 consumes the implemented tool without waiting for per-task verification.

Boundary:
Own the entire updater outcome: trusted source admission, full rendering with
each revision's own initializer/toolchain/lock, isolation preserving dirty
originals, native conflict visibility, generated-source and migration
admission, metadata-only acceptance, reachable baselines and recovery/no-op
semantics. Retain original `template.lock`; accepted `template.upgrade.json`
has the separate Design meaning. The guide includes the refusal and recovery
boundaries. No automatic profile migration, migration renumbering, runtime
ownership in portable sync, generic updater/journal or new dependency. Shared
input refactoring is allowed only where its existing contract is preserved.

Mutable owners:
- Upgrade CLI/library: `scripts/template-upgrade.sh`,
  `scripts/lib/template_upgrade.py`; smallest genuinely shared parsing in
  `template_state.py` / `template_init.py` under Design.
- Updater proof: `scripts/tests/template-upgrade.py`; existing initializer
  self-test/source checks and source-only inventory/routing only as required
  for the updater's complete generated-output closure.
- `docs/template-upgrade.md`, existing sync/README/command/structure owners
  for the supported route and links only.

Exclusive locks:
- Shared initializer library and source-only inventory/routing when mutated
  (`template_state.py`, `template_init.py`, `template_profiles.json`, existing
  source/self-test/classifier/make owners); serialize those edits with T2/T3.
- README/command/structure documentation when adding shared links.

Final validation:
- Claim: U1–U4 hold for the supported new/legacy consumer boundary, including
  full rendered truth, preservation/conflicts, accepted-baseline finality,
  repeatability and ordinary-clone custody. Migration/generated authority is
  preserved; unsupported or ambiguous inputs cannot become accepted.
- Checks: Consolidated changed-surface checks under repository validation and
  the focused proof required by the [upgrade design](../design/runtime-upgrades.md#idempotency-and-recovery).
  Implementation chooses cases and commands. Actual consumer A/B use belongs
  to T4's Completion evidence; do not add image/provider runs per updater case.
- Observable: Reviewed resolved content and accepted Git ancestry agree;
  failure/conflict/retry cannot mutate the original or fabricate acceptance.
  T4's business edits remain present through the accepted upgrade.

Reopen if:
Full historical rendering or required Git conflict/custody semantics prove
infeasible, input sharing changes initialization contracts, or a profile/schema
migration mechanism becomes necessary: return that smallest Technical Design
decision. Missing final tool/runtime capability alone is not a code blocker.
