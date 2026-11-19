# Implementation Review

Use as a fresh `reviewer-agent` on a fixed implementation candidate (assembled
ledger, single-unit delivery, or standalone review). Falsify independently and
return one evidence-bounded verdict without repairing, re-scoping, or
accepting.

Pin the brief's diff command (`git diff <Base>..HEAD` or supplied command) and
confirm the ref resolves and the diff is non-empty (`REVIEW_HANDOFF_INVALID`
otherwise). Inspect live source and direct callers within the assigned read scope before judging reachability. Reuse valid check receipts; run a bounded existing diagnostic only for a
concrete falsification question under [Evidence
Contract](../shared/evidence-contract.md#required-and-optional-proof).

## Read Scope And Standards

Use the assigned axes and the available accepted inputs. Apply Rust-specific standards only to Rust surfaces; for documentation or other changes use the owning policy and matching checks. Skip anything
deterministic tooling (`rustc`, `clippy`, `rustfmt`,
`quality/architecture.json`) already enforces.

| Mode | Read inputs | Standards to load |
| --- | --- | --- |
| Single reviewer (Axes 1–4) | Diff, accepted bundle, check receipts, production files + callers, tests | [Rust Testing](../../../.agents/skills/rust-testing/SKILL.md), [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md), and any domain skills matching a touched risk |
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
3. **Structure & Single Ownership.** Apply the deletion test and the changed surface's authoritative standards, including recorded decisions. Name the violated constraint and the action (`delete | collapse | bind | delegate <item>`).
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

Return [Review Result V1](../interfaces/review-result-v1.md). An invalid handoff has no verdict. Keep findings compact (target ≤400 words per lens), grouped by assigned axis:

```text
## Axis <N> — <Axis Name>
- none — attacked: <one line naming concrete scenario, probe, or mutated branch + catching test>
# OR per finding:
- [<TASK_DEFECT | INTEGRATION_DEFECT | UPSTREAM_GAP | FOLLOW_UP>] <path::Symbol> — violated: <exact anchor/rule>; trace: <reachable counterexample, surviving mutant, or structural action>; owner: <smallest owner>
```

Apply the classifications and routing in Review Result V1. Record missing optional observations in `evidence_boundary`.

## Complementary Panel

When [Review Trigger](../shared/review.md#trigger) selects a 2-lens panel, run
**Lens A** and **Lens B** (defined in [Read Scope And
Standards](#read-scope-and-standards)) in parallel on the frozen candidate.

**Synthesis (Delivery Owner):** `PASS` requires both lenses `PASS`; hold any
`FAIL` or `NEEDS_PARENT` verbatim per axis without reranking. Resolve an
evidence-backed lens conflict via `adjudicator-agent`.

## Delta Recheck

Inspect the pinned repair delta, repaired and open findings, verified partial refutations, repair decisions and invalidated proof. Cover all four axes on that delta:

- Falsify root-cause closure of each repaired finding.
- Falsify preserved spec parity and clean repair structure (`Engineering` and
  `Rust Change Surface`).
- Run the **Mutation Mental Test** on every new or edited regression test.

Keep unaffected closed reasoning and verified refutations intact.

## Structure-Only Review

Apply Axis 3 only to the structural additions named in the brief.
