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
- <exact spec.md#r<n>-... / spec.md#preserved-guarantees (G<n>) or design/design.md#d<n>-.../#s<n>-... anchor, verified path::Symbol, or non-obvious crate@version::item> — <rule or contract>

Provides:
- <exact compiling/behavioral contract consumed downstream, or none (leaf)>

Boundary:
- In scope: <primary responsibility, superseded cleanup, generated-contract regeneration, required durable doc update>
- Out of scope: <exclusions, sibling surfaces, local stop before later gates>

Mutable owners:
- <crate> — <subsystem>

Exclusive locks:
- <narrowest canonical repo-relative shared file or ordered directory mutated, or none>

Final validation:
- Observable claim: <public seam + base-commit-failing falsifier in assembled candidate; paired G<n> invariants>
- Additional proof: <source-mandated extra verification, or none>

Reopen if:
- <assumption, uncached crate check, or integration risk with reopen owner, or none>
```

## Field Rules

- **`Outcome` vs `Observable claim` vs `Completion`:** `Outcome` is unit-local
  when this task alone lands over its `Depends on`; `Observable claim` is the
  **base-commit-failing falsifier** Delivery Owner proves at the **public
  seam** in the assembled candidate (rejecting stubs/partial wiring, paired
  with `G<n>`). Cross-slice invariants and requested external outcomes live in
  `tasks.md`'s `Completion`.

  | Shape | `Outcome` (unit-local) | `Observable claim` (assembled) |
  | --- | --- | --- |
  | **Vertical slice** *(default)* | Seam-observable behavior delta (or withdrawn behavior for removals) | Trigger/state -> promised public-seam behavior (or withdrawn path unreachable) failing on base commit + paired `G<n>` |
  | **Behavior-preserving / quantitative** | Structural, ownership, or perf/resource delta keeping `G<n>` intact | Preserved `G<n>` + base-commit-failing structural witness (old path/callers gone, architecture rule) or measurable perf/resource target |
  | **Prefactor** (`[prefactor]`) | Existing-code cleanup preserving `G<n>` and unlocking an internal seam | Preserved `G<n>` + unlocked internal seam invariant (zero new feature behavior) |
  | **Shared contract** (`[shared-contract]`) | Minimal compiling contract/schema ahead of `≥2` parallel consumers | Compiling contract/schema and boundary invariants (consumer behavior lives downstream) |
  | **Conditional shapes** | Follow [Conditional Claim Rules](../phases/planning/conditional-branches.md#claim-rules). | Follow [Conditional Claim Rules](../phases/planning/conditional-branches.md#claim-rules). |

  - *Observable:* `POST /webhooks` with a tampered signature returns `401
    invalid_signature` and writes zero inbox rows (fails on base commit);
    paired `G1` valid-signature acceptance holds.
  - *Wrong layer:* `test_invalid_signature` passes, `make test` succeeds, or
    paraphrasing `Outcome` without a public-seam falsifier.
- **`Depends on` & `Provides` (producer isolation):** `Depends on` matches
  `tasks.md` ([Task Ledger
  V1](task-ledger-v1.md#authoring-schema-planning-owned)); put stops before
  later `[acceptance]` or `[effect:<action>]` gates in `Boundary -> Out of
  scope`. A producer Lead reads only its own packet, so whenever another task
  depends on `T<ID>`, `Provides` + `Sources` must pin every public type, trait,
  signature, error variant, config key, export, or schema invariant any
  dependent assumes (e.g., `infra_http::rate_limit::RateLimitLayer`,
  `RateLimitError::Exceeded { retry_after: Duration }` — not `"rate limiter"`).
  Write `none (leaf)` when no downstream task depends on `T<ID>`.
- **`Sources` & shared identifiers:** cite exact owned anchors (`spec.md#r1-...`,
  `spec.md#preserved-guarantees` naming `G1`, `design/design.md#d1-...`,
  `design/design.md#s1-...`, `rollout.md#...`) rather than bare whole-file
  links, plus verified `path::Symbol` and non-obvious `crate@version::item` (no
  line numbers, copied prose, or speculative new internal `.rs` paths). Link
  `design/design.md#<section>` when a sketch or table already lives there;
  inline a short snippet **only** when `design/` was skipped or Planning pins a
  new cross-task `shared-contract`/`prefactor` type. Pin any additional
  cross-task shared identifier once in `tasks.md`'s `Global constraints`.
- **`Mutable owners` & `Exclusive locks`:** scope `Mutable owners` by `<crate>
  — <subsystem>`. Lock the narrowest canonical repo-relative path concurrent
  writes collide on (concrete file or directory path, never globs or prose)
  across four categories (or `none`):
  1. *Manifests & policy:* `Cargo.toml`, `Cargo.lock`,
     `quality/architecture.json`, `scripts/template/profiles.json`,
     `make/template.mk`, `template-owned.paths`.
  2. *Ordered or generated surfaces:* `migrations/`, `.sqlx/`,
     `api/openapi/service.yaml`, `api/proto/`.
  3. *Shared wiring & module registries:* parent crate `lib.rs`/`mod.rs` when
     concurrent tasks add sibling modules or re-exports, plus shared router,
     state, config, or bootstrap wiring files.
  4. *Shared durable docs:* concrete files such as `docs/decisions/<topic>.md`,
     `docs/repo-architecture.md`, `docs/production-contract.md`,
     `docs/backend-library-selection.md`.
- **`Additional proof`:** Implementation owns proving layers, test cases,
  fixtures, and commands ([Validation budget](../../../AGENTS.md#validation-budget));
  write `none` unless an accepted source mandates extra proof ([Evidence
  Contract](../shared/evidence-contract.md#required-and-optional-proof)).
- **`Reopen if`:** carry applicable upstream `Assumptions and reopen triggers`,
  uncached crate checks, `integration-first` assumptions, and
  Implementation/Delivery Owner `CONCERNS` with reopen owners (or `none`). Any
  packet checklist is a non-canonical Lead lane, never a ledger task.
