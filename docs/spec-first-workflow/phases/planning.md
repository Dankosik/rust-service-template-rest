# Planning

Continue autonomously in the same session from `specs/<task>/spec.md` (and
`specs/<task>/design.md` if split). Turn closed decisions into executable
**tracer-bullet** vertical slices, clear one fresh [Plan Review](plan-review.md),
and stop at the ready handoff for a fresh Implementation session.

## Steps

1. **Ground blast radius and lock surfaces (static, read-only).**
   - Trace every touched symbol, trait bound, error path, superseded path/test,
     and [Exclusive lock](#exclusive-locks) surface across all workspace
     targets (including tests), checking callers and match sites of changed
     signatures or exhaustive public types.
   - Verify non-obvious `Cargo.lock` crate APIs offline (`cargo metadata
     --locked --offline --format-version 1`). Offload wide multi-crate sweeps to
     `evidence-agent`.
   - On reopen of an existing ledger, keep unaffected task IDs and integrated
     `[x]` work against its original `Base:` intact.
   - If zero coding delta remains and every accepted obligation already holds on
     primary sources (with no integrated work awaiting acceptance), verify that
     claim via [Plan Review](plan-review.md#zero-delta-check) and report the
     verified result to the user without writing a ledger.
   - *Done when:* every obligation (`R<n>`, `G<n>`, `D<n>`, `S<n>`, rollout
     gates, durable doc updates, superseded cleanup) and affected caller is
     mapped.

2. **Choose the smallest coherent task graph.**
   - Apply [Task Shapes And Granularity](#task-shapes-and-granularity). Default
     to **1 task** whenever one fresh Lead holds the change in its **smart
     zone** (one coherent outcome, bounded crate set, uniform gate boundary).
   - Keep architecture, profile, manifest, and durable doc updates atomic with
     their landing slice, and cover every broken caller in the changing task or
     a compatible staged refactor.
   - Apply the deletion test to multi-task cuts: merge coupled pairs that
     serialize on shared locks, pull forward risk retirement or a prefactor
     simplifying `≥2` dependents, and remove any task whose deletion loses no
     outcome or prerequisite. Keep post-deploy `multi-release` `Contract` steps
     outside Release 1's ledger.
   - *Done when:* every task has a clear vertical boundary, minimal lock
     contention, and compiles/verifies cleanly on top of its coding
     prerequisites.

3. **Write `specs/<task>/tasks.md` and `specs/<task>/tasks/<ID>-<slug>.md`.**
   - Persist `tasks.md` at `status: draft` and one packet per task (including
     for a 1-task ledger) using [Ledger And Packet Format](#ledger-and-packet-format).
   - Remove superseded orphan packets and clear Orchestrator runtime fields on a
     fresh draft with no integrated `[x]` tasks.
   - Verify on disk before review: topologically ordered DAG with no cycles,
     every `R<n>`/`D<n>`/`S<n>` traced, producer `Provides` pinning exact types
     and signatures consumed downstream, zero undeclared lock or
     exhaustive-type collisions across concurrently eligible pairs, and a
     shape-appropriate `Observable claim` per task (base-commit-failing
     public-seam witness for vertical slices, or the matching shape rule below).
   - *Done when:* the on-disk bundle passes every structural and cold-start
     check.

4. **Run fresh [Plan Review](plan-review.md) and hand off.**
   - Dispatch fresh reviewer(s) on the fixed on-disk bundle (`spec.md`,
     optional `design.md` and `intent.md`, `tasks.md`, `tasks/*.md`, plus
     `research/*.md` for Axis 1) under [Review
     Lifecycle](../shared/review.md#lifecycle).
   - Verify every finding against primary sources. Repair confirmed defects in
     place (`spec.md`, `design.md`, or packets) and re-run a bounded delta
     recheck (or fresh review if resliced or interfaces changed). Escalate to
     the user only for a confirmed product/risk contradiction requiring a user
     decision.
   - Once review clears (`PASS` or bounded `CONCERNS` recorded in `Global
     constraints` / packet `Reopen if`), flip `spec.md`, `design.md` (if
     present), and `tasks.md` to `status: ready`, and stop at [Implementation
     Handoff](../shared/transition.md#implementation-handoff).
   - *Done when:* `tasks.md` is `status: ready`, review cleared, and the user
     has the one-screen plan overview and fresh-session launch message.

## Task Shapes And Granularity

- **Split** only for: (1) independent observable outcomes compiling and
  verifying alone; (2) genuine parallelism across disjoint `Mutable owners`,
  `Exclusive locks`, and exhaustive-type updates; (3) isolating an unverified
  integration seam or external gate; (4) smart-zone or blast-radius overload
  across independent subsystems; or (5) a prerequisite shape below.
- **Merge** tasks of one outcome sharing an `Exclusive lock` or `Mutable owner`
  unless risk/gate isolation (3), smart-zone overload (4), or a prerequisite
  shape (5) applies; tasks split solely for concurrency that serialize on a
  lock; horizontal layers (`types -> repo -> handler -> tests`); or standalone
  cleanup/docs tails (fold into the landing slice, except `Contract`).

| Shape | Tag | When to use | Guardrail |
| --- | --- | --- | --- |
| **Vertical slice (tracer bullet)** | *(default; omit)* | Narrow, complete path through touched layers delivering observable behavior (or a behavior-preserving/quantitative delta) at its seam alone. | Litmus test: *"With prerequisites met and other tasks deferred, what works or is provably changed at the seam now?"* |
| **Prefactor** | `[prefactor]` | Behavior-preserving cleanup of *existing* code ordered first to simplify the next slice(s). | Compiles alone; zero new feature types, schemas, or behavior. |
| **Shared contract** | `[shared-contract]` | Minimal compiling contract/schema (types, traits, proto, CLI/`just` contract, migration) ordered first. | Valid **only** when `≥2` lock-disjoint downstream slices run in parallel after it lands; otherwise fold into the first vertical slice. |
| **Integration-first** / **Wide refactor** | `[integration-first]` / `[expand]`, `[migrate]`, `[contract]` | Unverified external seam is the primary risk, or blast radius prevents one atomic green task. | Load [Conditional Planning Branches](planning/conditional-branches.md) when triggered. |

## Ledger And Packet Format

### `specs/<task>/tasks.md`

```markdown
# Goal: <short request outcome>
status: draft | ready | blocked | done
Blocked stop: <missing input/decision and owner; omit unless blocked>
Completion: <falsifiable observable condition for the assembled request, plus any requested external outcome>
Global constraints: <active rules, cross-task pinned identifiers, already-holds anchors, or carried CONCERNS; omit when none>

## Tasks

- [ ] T<ID>: <one-line observable outcome> [<shape; omit for vertical slice>]
  - Depends on: <T<ID> [stage], <gate> [effect:<action>], or none>
  - Packet: tasks/<ID>-<slug>.md
```

- Order tasks **topologically** and by dispatch priority (risk retirement,
  `prefactor`, `shared-contract`, and critical-path blockers first).
- **Stage syntax:** unannotated `T1` gates coding on `T1`'s integrated code
  (required whenever compiling against a type, trait, config field, exhaustive
  type, dependency, or SQLx schema from `T1`); `T1 [acceptance]` gates final
  verification only; `<gate> [effect:<action>]` gates an external action.
- Runtime fields (`Base:`, `[x]`, receipts) are owned by Implementation under
  [Acceptance Result V1](../interfaces/acceptance-result-v1.md#ledger-runtime-state).

### `specs/<task>/tasks/<ID>-<slug>.md`

```markdown
# T<ID> — <short observable outcome>
Shape: <shape; omit for vertical slice>

Outcome:
<current -> target delta observed at this task's seam when this task alone lands>

Depends on:
- <T<ID> [stage] — consumed seam/gate, or none>

Sources:
- <exact spec.md#r1-... / design.md#d1-... anchor, verified path::Symbol, or crate@version::item> — <rule or contract>

Provides:
- <exact compiling/behavioral contract consumed downstream (types, signatures, errors, keys), or none (leaf)>

Boundary:
- In scope: <primary responsibility, superseded cleanup, generated contracts, required durable doc update>
- Out of scope: <exclusions, sibling surfaces, local stop before later gates>

Mutable owners:
- <crate> — <subsystem>

Exclusive locks:
- <narrowest canonical repo-relative shared file or ordered directory mutated, or none>

Final validation:
- Observable claim: <public seam + base-commit-failing falsifier in assembled candidate; paired G<n>>
- Additional proof: <source-mandated extra verification, or none>

Reopen if:
- <assumption, uncached crate check, or integration/concern trigger with owner, or none>
```

- **`Outcome` vs `Observable claim` vs `Completion`:** `Outcome` is unit-local
  when `T<ID>` lands over its `Depends on`; `Observable claim` is proved on the
  assembled candidate (for default vertical slices, a base-commit-failing
  public-seam falsifier such as `POST /webhooks` with a tampered signature
  returns `401 invalid_signature` and writes zero inbox rows, paired with `G1`,
  leaving test names and commands to Implementation; for `[prefactor]`,
  preserved `G<n>` + unlocked internal seam; for `[shared-contract]`, compiling
  contract and boundary invariants; for `[integration-first]` / `[expand]` /
  `[migrate]` / `[contract]`, follow [Conditional Claim
  Rules](planning/conditional-branches.md#claim-rules)); cross-slice and
  external outcomes live in `tasks.md`'s `Completion`.
- **Producer isolation:** a Unit Lead reads only its own packet, so when
  another task depends on `T<ID>`, `Provides` + `Sources` must pin every
  consumed symbol, signature, error variant, config key, and invariant. Cite
  exact anchors (no bare `spec.md`/`design.md` links in multi-task ledgers) and
  keep IDs, shapes, and `Depends on` identical between `tasks.md` and packets.

### Exclusive Locks

List concrete repo-relative paths (files or directories) across four categories:

1. **Manifests & policy:** `Cargo.toml`, `Cargo.lock`,
   `quality/architecture.json`, `template/profiles.json`, `justfile`.
2. **Ordered or generated surfaces:** `migrations/`, `.sqlx/`,
   `api/openapi/service.yaml`, `api/proto/`.
3. **Shared wiring & registries:** parent crate `lib.rs`/`mod.rs` when parallel
   tasks add sibling modules or re-exports, plus shared router, state, config,
   or bootstrap wiring files.
4. **Shared durable docs:** concrete files under `docs/decisions/`,
   `docs/repo-architecture.md`, `docs/production-contract.md`, or
   `docs/backend-library-selection.md`.
