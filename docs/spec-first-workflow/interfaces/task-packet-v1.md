# Task Packet V1

Every task in `tasks.md` has one packet at `specs/<task>/tasks/<ID>-<slug>.md`:

```markdown
# T<ID> — <short observable outcome>
Shape: <prefactor | shared-contract | integration-first | expand | migrate | contract; omit for vertical slice>

Outcome:
<current -> target delta observed at this task's seam when this task alone lands>

Depends on:
- <T<ID> [stage] — consumed seam/gate, <external gate> [effect:<action>], or none>

Sources:
- <path#section, verified path::Symbol, or non-obvious crate@version::item> — <rule or contract>

Provides:
- <exact behavior, compiling contract, type/trait/schema, error mode, or export consumed downstream or by Completion>

Boundary:
- In scope: <primary responsibility, superseded cleanup, required durable contract or decision-doc update>
- Out of scope: <exclusions, sibling surfaces, local stop before later gates>

Mutable owners:
- <semantic owner: crate + subsystem>

Exclusive locks:
- <narrowest canonical repo-relative shared file or ordered directory mutated, or none>

Final validation:
- Observable claim: <public seam + base-commit-failing behavior in the assembled candidate; paired G<n> invariants>
- Additional proof: <source-mandated extra verification, or none>

Reopen if:
- <assumption, uncached crate check, or integration risk with reopen owner, or none>
```

## Field Rules

- **`Outcome` vs `Observable claim` vs `Completion`:**
  - `Outcome` is the unit-local delta verifiable when this task lands alone
    over its `Depends on`.
  - `Observable claim` is what Delivery Owner proves at the **public seam** in
    the **assembled candidate**: it fails on the base commit and rejects a stub
    or partial wiring. Pair preserved guarantees (`G<n>`) as non-regression
    invariants alongside the failing delta, never as a feature task's sole
    claim.
  - Shape claims: **removal** / **contract** names the withdrawn behavior, old
    form, or shim and entry point/compile boundary; **prefactor** names the
    preserved invariant and unlocked internal seam; **shared-contract** names
    the compiling contract/schema and its boundary invariants (consumer
    behavior lives on downstream slices); **expand** keeps dual-form
    compatibility in `Outcome` and either proves new-form + old-caller
    compatibility in `Observable claim` (when `contract` is in a later release)
    or writes `superseded at assembly by T<contract>` (when `contract` removes
    the temporary shim in the same ledger); **migrate** names preserved
    behavior (`G<n>`) and zero remaining old-form references inside the batch's
    `Mutable owners`.
  - Cross-slice invariants and requested external outcomes live in `tasks.md`'s
    `Completion`.
- **`Depends on` & `Provides` (producer isolation):** `Depends on` matches
  `tasks.md` IDs and stage syntax ([Task Ledger
  V1](task-ledger-v1.md#authoring-schema-planning-owned)); put stops before
  later `[acceptance]` or `[effect:<action>]` gates in `Boundary -> Out of
  scope`. A producer Lead reads only its own packet, so `Provides` + `Sources`
  must pin every type, signature, error mode, export, or schema invariant any
  dependent assumes.
- **`Sources` & shared identifiers:** link accepted anchors (`spec.md#r1-...`,
  `design/design.md#d1-...`, `design/design.md#grounding-and-planning-handoff`,
  `rollout.md#...`), verified existing `path::Symbol`, and non-obvious
  `crate@version::item` items; omit line numbers, copied prose, and speculative
  new internal `.rs` file paths. Keep exact domain terms and `Pinned shared
  identifiers` from `spec.md`, `design/`, and code; pin any additional
  cross-task shared identifier once in `tasks.md`'s `Global constraints`.
  Inline a short snippet only when a prototype or design pins a type shape,
  state table, or wire vector more precisely than prose.
- **`Mutable owners` & `Exclusive locks`:** scope `Mutable owners` by crate and
  subsystem rather than brittle file lists. Lock the **narrowest canonical
  repo-relative shared file or ordered directory** concurrent writes collide
  on—for example `Cargo.toml`, `Cargo.lock`, `api/openapi/service.yaml`,
  `api/proto/`, `migrations/`, `.sqlx/`, `quality/architecture.json`,
  `make/template.mk`, `template-owned.paths`, `scripts/template/profiles.json`,
  shared `docs/decisions/*.md`, or specific shared wiring files
  (`crates/config/src/app.rs`, `crates/config/src/lib.rs`,
  `crates/service/src/api.rs`, `crates/service/src/state.rs`,
  `crates/service/src/bootstrap/mod.rs`,
  `crates/jobs-worker/src/bootstrap/mod.rs`, `crates/migrate/src/lib.rs`)
  rather than a whole crate directory when tasks edit disjoint subsystem
  modules. Write `none` otherwise.
- **`Additional proof`:** Implementation owns proving layers, test cases,
  fixtures, and commands ([Validation budget](../../../AGENTS.md#validation-budget)).
  Write `none` unless an accepted source mandates extra proof (load [Evidence
  Contract](../shared/evidence-contract.md#required-and-optional-proof) only to
  reconcile mandated additions or external gates). Optional infrastructure is a
  limitation, not a gate.
- **`Reopen if`:** carry applicable upstream `Assumptions and reopen triggers`
  (`spec.md`, `design/`), uncached crate checks, unverified integration risks,
  and bounded review concerns affecting this task—each with its reopen owner;
  write `none` when empty. Any checklist inside a packet is a non-canonical
  Lead lane, never a ledger task.
