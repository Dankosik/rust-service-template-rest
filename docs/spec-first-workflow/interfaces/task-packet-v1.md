# Task Packet V1

Every task in `tasks.md` has one packet at `specs/<task>/tasks/<ID>-<slug>.md`:

```markdown
# T<ID> — <short outcome>

Outcome:
<current -> target delta: what fails or is absent on base commit, and what holds once Implemented>

Depends on:
- <T<ID> [stage] — consumed output/gate, <external gate> [effect:<action>], or none>

Sources:
- <repo-relative path#section, verified path::Symbol, or crate@version::item> — <rule, decision, or API used here>

Provides:
- <consumable behavior, contract, or seam referenced downstream or by Completion>

Boundary:
- In scope: <primary responsibility and required replacement/cleanup of superseded code or tests>
- Out of scope: <material exclusions and sibling-owned surfaces left untouched>

Mutable owners:
- <concrete semantic owner, e.g. crate + subsystem, specific enough to prove disjointness>

Exclusive locks:
- <shared surface actually mutated — e.g. Cargo.toml/Cargo.lock, api/openapi.yaml, migrations/, quality/architecture.json, crates/service bootstrap, crates/config — or none>

Final validation:
- Observable claim: <what result at the affected entry point distinguishes accepted behavior from a plausible incomplete change and fails on the base commit?>
- Additional proof: <only extra verification explicitly mandated by an accepted source with anchor, or none>

Reopen if:
- <specific bounded assumption, uncached crate check, or upstream decision with owner, or none>
```

## Field Rules

- **Outcome vs Observable claim:** `Outcome` states what this unit changes
  locally; `Observable claim` states what holds after assembly. For a removal,
  name the withdrawn behavior and entry point where it ceases; for a prefactor
  or rename, name the preserved invariant and what it unlocks.
- **Depends on:** blocking task edges and external gates only, matching
  `tasks.md` IDs and [Task Ledger V1](task-ledger-v1.md#authoring-schema-planning-owned)
  stage syntax. Later `[acceptance]` or `[effect:<action>]` gates never block
  local coding; state the local stop boundary in `Boundary`.
- **Sources:** narrow context pointers (`path#section`, cross-repo name when
  external) plus source-verified `path::Symbol` / `crate@version::item` anchors
  from the Readiness Dry Run—never fragile line numbers or copied spec prose.
  Inline a compact snippet only when an accepted prototype or design encodes an
  exact type shape, state machine, or wire vector more precisely than prose.
- **Mutable owners & Exclusive locks:** concrete enough for concurrent dispatch
  without guessing a brittle file list. List only shared surfaces this unit
  actually mutates (no precautionary locks); write `none` otherwise.
- **Proof boundary:** Implementation owns proving layers, test cases, fixtures,
  and commands under [Validation budget](../../../AGENTS.md#validation-budget)
  and [Evidence Contract](../shared/evidence-contract.md#required-and-optional-proof).
  Packets cannot expand local acceptance—write `none` in `Additional proof`
  unless an accepted source requires more. Optional test infrastructure is a
  limitation, not a gate.
- **Reopen if:** concrete assumptions only; write `none` rather than generic
  boilerplate ("if the spec changes").
- Optional checklists inside a packet are non-canonical execution lanes, never
  ledger tasks.
