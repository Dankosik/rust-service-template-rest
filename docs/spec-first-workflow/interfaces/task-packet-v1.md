# Task Packet V1

Every task in `tasks.md` has one packet at `specs/<task>/tasks/<ID>-<slug>.md`:

```markdown
# T<ID> — <short observable outcome>

Outcome:
<current -> target delta observed when this unit alone lands>

Depends on:
- <T<ID> [stage] — consumed output/gate, <external gate> [effect:<action>], or none>

Sources:
- <path#section, verified path::Symbol, or crate@version::item> — <rule, decision, or API contract>

Provides:
- <concrete behavior, contract, or seam handle consumed downstream or by Completion>

Boundary:
- In scope: <primary responsibility, superseded code/test cleanup, required durable doc update>
- Out of scope: <exclusions, sibling surfaces, local stop before later gates>

Mutable owners:
- <concrete semantic owner, e.g. crate + subsystem>

Exclusive locks:
- <canonical repo-relative shared path/prefix mutated — e.g. Cargo.toml, Cargo.lock, api/openapi/service.yaml, api/proto/, migrations/, .sqlx/, quality/architecture.json, crates/config/, crates/service/src/, make/template.mk, template-owned.paths, scripts/template/profiles.json — or none>

Final validation:
- Observable claim: <public seam + falsifiable behavior failing on base commit and distinguishing accepted behavior from a plausible incomplete change; list preserved G<n> invariants alongside>
- Additional proof: <extra verification mandated by an accepted source anchor, or none>

Reopen if:
- <bounded assumption, uncached crate check, or integration assumption with owner, or none>
```

## Field Rules

- **Outcome vs Observable claim vs Completion:** `Outcome` states the local
  delta verifiable when this unit lands (over its declared `Depends on`,
  without downstream or sibling tasks). `Observable claim` names the **public
  seam / entry point** and behavior Delivery Owner verifies in the assembled
  candidate; it must fail on the base commit and distinguish accepted behavior
  from a plausible incomplete change without downstream or sibling tasks. For a
  **removal**, name the withdrawn behavior and entry point; for a **prefactor**
  or **expand** step, name the preserved invariant and unlocked seam or
  compatible form. State **preserved consumer guarantees** (`G<n>`) as
  non-regression invariants alongside a base-commit-failing delta, never as a
  feature task's sole claim. Cross-slice properties belong in `tasks.md`'s
  `Completion`.
- **Depends on:** blocking edges and external gates only, matching `tasks.md`
  IDs and [Task Ledger V1](task-ledger-v1.md#authoring-schema-planning-owned)
  stage syntax. Later `[acceptance]` or `[effect:<action>]` gates leave local
  coding unblocked; put the local stop in `Boundary -> Out of scope`.
- **Sources & terminology:** use narrow pointers (`spec.md#r1-...`,
  `design/...#section`, external repo name) and source-verified `path::Symbol`
  / `crate@version::item` anchors instead of line numbers or copied prose.
  Preserve exact domain terms, config keys, error codes, and shared identifiers
  from `spec.md`, `design/`, and code so parallel Leads converge on identical
  names. Inline a compact snippet only when a prototype or design encodes an
  exact type shape, state table, or wire vector more precisely than prose.
- **Mutable owners & Exclusive locks:** scope `Mutable owners` tightly enough
  for concurrent dispatch without a brittle file list. Express `Exclusive
  locks` as **canonical repo-relative paths or prefixes** (including shared
  registration files such as a shared crate `lib.rs` or router wiring) so
  colliding tasks carry identical tokens. List only mutated shared surfaces;
  write `none` otherwise.
- **Proof boundary:** Implementation owns proving layers, test cases, fixtures,
  and commands under [Validation budget](../../../AGENTS.md#validation-budget).
  Write `none` in `Additional proof` unless an accepted source explicitly
  mandates more (load [Evidence
  Contract](../shared/evidence-contract.md#required-and-optional-proof) only to
  reconcile mandated additions or external gates). Optional test infrastructure
  is a limitation, not a gate.
- **Reopen if:** concrete assumptions with reopen owners only; write `none`
  instead of boilerplate. Optional checklists inside a packet are non-canonical
  execution lanes, never ledger tasks.
