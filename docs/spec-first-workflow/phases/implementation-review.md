# Implementation Review

Use as a fresh `reviewer-agent` on a fixed implementation candidate (assembled
ledger, single-unit delivery, or standalone review). Falsify independently and
return one evidence-bounded verdict without repairing, re-scoping, or
accepting.

Pin the brief's diff command (`git diff <Base>..HEAD` or supplied command) and
confirm the ref resolves and the diff is non-empty (`REVIEW_HANDOFF_INVALID`
otherwise). Read touched production files, surrounding modules, and direct
callers before reading tests — judge live code in context, not diff hunks
alone. Reuse valid check receipts; run a bounded existing diagnostic only for a
concrete falsification question under [Evidence
Contract](../shared/evidence-contract.md#required-and-optional-proof).

## Read Scope And Standards

Load only the sources and `.agents/skills/rust-*` standards assigned to your
review mode so parallel lenses stay disjoint and lean. Skip anything
deterministic tooling (`rustc`, `clippy`, `rustfmt`,
`quality/architecture.json`) already enforces.

| Mode | Read inputs | Standards to load |
| --- | --- | --- |
| Single reviewer (Axes 1–4) | Diff, accepted bundle, check receipts, production files + callers, tests | [Rust Testing](../../../.agents/skills/rust-testing/SKILL.md), [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md), plus 1–2 matching domain `rust-*` skills below |
| Lens A — Spec, Scope & Proof (Axes 1 + 4) | Diff, `spec.md`, `tasks.md`, packets, check receipts, tests, and touched production code strictly for reachability and Mutation Mental Test | [Rust Testing](../../../.agents/skills/rust-testing/SKILL.md); add `rust-api-contract` if HTTP/OpenAPI changed |
| Lens B — Runtime, Seams & Structure (Axes 2 + 3) | Diff, touched production files + surrounding modules + callers, `design/`, `rollout.md`, task `decisions` (skip `intent.md` and full test suites unless verifying a seam caller) | [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md), plus 1–2 matching domain skills for touched risk surfaces (`rust-sqlx`, `rust-tokio`, `rust-security`, `rust-axum`, `rust-reliability`, `rust-errors`, `rust-config`) |
| [Structure-Only](#structure-only-review) (Axis 3) | Diff hunks and callers for each structural addition named in the brief | [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md) |

## Adversarial Falsifiers

Evaluate every assigned axis under explicit per-axis headings so one passing
axis never masks another's defect.

1. **Outcome, Spec & Scope Parity.** Start from accepted sources and
   `Completion`. Construct a concrete caller or operator scenario where every
   executed test stays green yet an accepted obligation (`R<n>`), preserved
   `G<n>`, withdrawn removal path, or `Completion` criterion fails. Trace every
   material hunk to an accepted obligation; flag unrequested behavior or silent
   scope drift, citing the exact violated anchor (`spec.md#R<n>`,
   `T<ID> Observable claim`, or `Completion`).
2. **Real-Path Defects & Cross-Unit Seams.** Attack touched production code
   with the loaded domain `rust-*` skills and five probes:
   - *Precedence & rejection:* colliding rules or errors; state mutation or
     info leak before rejection.
   - *Partial failure & cancellation:* future dropped mid-`.await`, halfway
     write failure, timeout, held lock/permit, non-idempotent retry, or
     irreversible migration.
   - *Silent degradation:* `let _ =`, `.ok()`, catch-all arms, or defaults
     turning real failures into silent success.
   - *Resource bounds & privacy:* unbounded untrusted allocation, batch size,
     or task spawning; high-cardinality metric labels; or secret/PII leakage.
   - *Cross-unit seams & sibling drift:* compare types, error codes, units,
     defaults, and `decisions` across `Provides -> Depends on` seams
     (`INTEGRATION_DEFECT`); flag duplicated private helpers or divergent
     vocabulary, error shapes, or telemetry fields across parallel tasks.
3. **Structure & Single Ownership.** Apply `rust-structural-quality`'s
   **deletion test**, [Engineering](../../../AGENTS.md#engineering), and [Rust
   Change Surface](../../../AGENTS.md#rust-change-surface). Flag reimplementing
   `std`, a locked `Cargo.lock` crate, or an authoritative tool; splitting one
   fact across hand-synced carriers; adding an unjustified crate,
   dependency/feature, layer, trait, config key, or fallback/shim; or violating
   non-clippy repo rules (explanatory comments, `#[non_exhaustive]` on public
   workspace types, debug probes/logging, unjustified policy waivers). **Repo
   standards override generic taste:** suppress objections endorsed by
   `AGENTS.md` or `docs/decisions/`. Name the violated constraint and exact
   action (`delete | collapse | bind | delegate <item>`).
4. **Test & Proof Falsifiability.** Apply the **Mutation Mental Test** to every
   added or edited test: mentally invert or delete the single production branch
   it claims to guard — would it go **red**? Flag **tautological** assertions
   (recomputing expected values like production code), mocking the claimed
   boundary, `is_ok()`-only or internal-step assertions, sleeps/retries masking
   flakes, duplicate same-layer suites, and deleted or weakened safety,
   durability, or sole regression guards. Name the exact surviving mutant
   (`inverting/deleting <expr> leaves <test> green`).

## Blocking Bar And Output Format

**Verify reachability before blocking:** confirm in live callers and callees
that any blocking counterexample is reachable (not blocked by an upstream type
invariant, validator, or middleware). **Return `PASS` cleanly when no reachable
counterexample survives; never invent speculative findings to look thorough.**

Record compact `evidence_boundary` reconstructions for assigned axes:
- **Axes 1 / 4:** obligations traced (`R<n>`, `G<n>`, `Observable claim`,
  `Completion`) and concrete production branches mentally mutated against
  guarding tests.
- **Axes 2 / 3:** production files, callers, and cross-unit seams inspected,
  plus `.agents/skills/rust-*` skills loaded.

Return [Review Result V1](../interfaces/review-result-v1.md) (`PASS | FAIL |
NEEDS_PARENT`, or `REVIEW_HANDOFF_INVALID`), keeping `findings` compact
(target **≤400 words per lens**, structured bullets only, no prose essays):

```text
## Axis <N> — <Axis Name>
- none — attacked: <one line naming concrete scenario, probe, or mutated branch + catching test>
# OR per finding:
- [<TASK_DEFECT | INTEGRATION_DEFECT | UPSTREAM_GAP | FOLLOW_UP>] <path::Symbol> — violated: <exact anchor/rule>; trace: <reachable counterexample, surviving mutant, or structural action>; owner: <smallest owner>
```

Blocking findings (`TASK_DEFECT`, `INTEGRATION_DEFECT`, `UPSTREAM_GAP`, and
real in-scope defects from optional checks) require all four fields. Style
preferences, equivalent single-owner structure, naming polish, speculative
risks, untouched historical debt, and missing optional infrastructure stay
`FOLLOW_UP` and never block `PASS`.

## Complementary Panel

When [Review Trigger](../shared/review.md#trigger) selects a 2-lens panel, run
**Lens A** and **Lens B** (defined in [Read Scope And
Standards](#read-scope-and-standards)) in parallel on the frozen candidate.

**Synthesis (Delivery Owner):** `PASS` requires both lenses `PASS`; hold any
`FAIL` or `NEEDS_PARENT` verbatim per axis without reranking. Resolve an
evidence-backed lens conflict via `adjudicator-agent`. Post-repair recheck uses
one fresh [Delta Recheck](#delta-recheck) reviewer.

## Delta Recheck

Run **one fresh `reviewer-agent`** scoped to the brief's pinned repair diff,
repaired/open anchored findings, Delivery-Owner-verified partial-refutation
anchors, repair `decisions`, and invalidated proof (all four axes on the repair
delta, including after a 2-lens Panel):

- Falsify root-cause closure of each repaired finding.
- Falsify preserved spec parity and clean repair structure (`Engineering` and
  `Rust Change Surface`).
- Run the **Mutation Mental Test** on every new or edited regression test.

Keep unaffected closed reasoning and verified refutations intact.

## Structure-Only Review

Run **Falsifier 3** exclusively on each structural addition named in the brief,
keeping behavior and proof outside scope. Classify
[Engineering](../../../AGENTS.md#engineering) and [Rust Change
Surface](../../../AGENTS.md#rust-change-surface) violations as `TASK_DEFECT`
naming the exact action (`delete | collapse | bind | delegate <item>`);
classify other observations as `FOLLOW_UP`.
