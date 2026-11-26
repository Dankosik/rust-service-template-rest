# Implementation Review

Fresh, read-only review of one fixed implementation candidate (assembled ledger,
Direct Work, or standalone review). Falsify independently and return [Review
Result](../shared/review.md#review-result) without repairing, re-scoping, or
accepting.

Pin the brief's diff command (`git diff <Base>..HEAD` or supplied command) and
confirm the ref resolves with a non-empty diff (return `REVIEW_HANDOFF_INVALID`
otherwise). Inspect live source and direct callers within the assigned read
scope before judging reachability. Reuse valid check receipts; run a bounded
diagnostic only for a concrete falsification question under [Evidence
Contract](../shared/evidence-contract.md#required-and-optional-proof).

## Read Scope And Standards

Apply Rust-specific skills only to Rust surfaces; for docs or other surfaces use
their owning policy. Skip checks deterministic tooling (`rustc`, `clippy`,
`rustfmt`, `quality/architecture.json`) already enforces.

| Mode | Read inputs | Standards to load |
| --- | --- | --- |
| Single reviewer (Axes 1–4) | Diff, accepted bundle, check receipts, production files + callers, tests | [Rust Testing](../../../.agents/skills/rust-testing/SKILL.md), [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md), [Rust Idiomatic](../../../.agents/skills/rust-idiomatic/SKILL.md), and domain skills matching touched risks |
| Lens A — Spec, Scope & Proof (Axes 1 + 4) | Diff, `spec.md`, `tasks.md`, packets, check receipts, tests, and touched production code strictly for reachability and Mutation Mental Test | [Rust Testing](../../../.agents/skills/rust-testing/SKILL.md); add `rust-api-contract` if HTTP/OpenAPI changed |
| Lens B — Runtime, Seams & Structure (Axes 2 + 3) | Diff, touched production files + surrounding modules + callers, `design.md` (or `spec.md#architecture` when inline), task `decisions` | [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md), [Rust Idiomatic](../../../.agents/skills/rust-idiomatic/SKILL.md), plus 1–2 matching domain skills for touched risks (`rust-sqlx`, `rust-tokio`, `rust-security`, `rust-axum`, `rust-reliability`, `rust-errors`, `rust-config`; add `rust-dependencies` or `rust-performance` when adding data structures, parsers, caches, or editing hot paths) |
| [Structure-Only](#structure-only-review) (Axis 3) | Diff hunks and callers for each structural addition named in the brief | [Rust Structural Quality](../../../.agents/skills/rust-structural-quality/SKILL.md), [Rust Idiomatic](../../../.agents/skills/rust-idiomatic/SKILL.md) |

## Adversarial Falsifiers

Evaluate every assigned axis under explicit per-axis headings so one passing
axis never masks another's defect:

1. **Outcome, Spec & Scope Parity.** Start from `spec.md` and `Completion`.
   Construct a concrete caller or operator scenario where every executed test
   stays green yet an accepted `R<n>`, preserved `G<n>`, hard external
   constraint, `Must not` prohibition, withdrawn path, or `Completion` target
   fails. Trace every material hunk to an accepted obligation and flag silent
   scope drift with exact anchors.
2. **Real-Path Defects & Cross-Unit Seams.** Attack touched production code
   with the loaded domain skills and five probes:
   - *Precedence & rejection:* colliding rules or errors; state mutation or info
     leak before rejection.
   - *Partial failure & cancellation:* future dropped mid-`.await`, halfway
     write failure, timeout, held lock/permit, non-idempotent retry, or
     irreversible migration.
   - *Silent degradation:* `let _ =`, `.ok()`, catch-all arms, or defaults
     turning real failures into silent success.
   - *Resource bounds & privacy:* unbounded untrusted allocation, batch size, or
     task spawning; high-cardinality metric labels; secret/PII leakage.
   - *Cross-unit seams & sibling drift:* compare types, error codes, units,
     defaults, and `decisions` across `Provides -> Depends on` seams
     (`INTEGRATION_DEFECT`); flag duplicated helpers or divergent error shapes
     and telemetry across parallel tasks.
3. **Structure & Single Ownership.** Apply the deletion test, deep-module
   standards, and behind-the-seam type/readability checks against touched code
   and recorded decisions:
   - *Type & ownership invariants (`rust-idiomatic`):* illegal or inert
     `Option`/`bool` combinations, raw primitives where `NonZero*` or newtypes
     prevent bugs, repeated runtime validation instead of boundary parsing, and
     needless owned/cloning shapes on transient per-call descriptors.
   - *Delegation & exhaustiveness (`rust-dependencies`, `rust-coder`):*
     hand-rolled collections, parsers, or caches already covered by
     `workspace-crates.md`, and catch-all `_ =>` arms hiding additions to closed
     workspace or upstream enums.
   - *Role clarity (`rust-structural-quality`):* functions mixing distinct roles
     or hiding invariants behind unclear names. Name the violated constraint and
     concrete action (`delete | collapse | bind | delegate | narrow-type <item>`).
4. **Test & Proof Falsifiability.** Run the **Mutation Mental Test** on every
   added or edited test: mentally invert or delete the single production branch
   it claims to guard—would it go **red**? Flag tautological assertions, mocking
   the claimed boundary, `is_ok()`-only or internal-step assertions, sleeps
   masking flakes, duplicate same-layer suites, and weakened safety or
   regression guards, naming the surviving mutant (`inverting/deleting <expr>
   leaves <test> green`).

## Output Format

Verify reachability in live callers and callees before blocking: return `PASS`
when no reachable counterexample survives, `FAIL` for any witnessed
`TASK_DEFECT`, `INTEGRATION_DEFECT`, or invalid accepted `UPSTREAM_GAP`, or
`NEEDS_PARENT` when missing authoritative input or unavailable required proof
prevents closing the review. Keep findings compact (target ≤400 words per
lens), grouped by axis:

```text
## Axis <N> — <Axis Name>
- none — attacked: <concrete scenario, probe, or mutated branch + catching test>
# OR per finding:
- [<TASK_DEFECT | INTEGRATION_DEFECT | UPSTREAM_GAP | FOLLOW_UP>] <path::Symbol> — violated: <anchor/rule>; trace: <reachable counterexample, surviving mutant, or structural action>; fix: <minimal concrete replacement or signature sketch>; owner: <smallest owner>
```

Record compact `evidence_boundary` summaries (obligations traced and mutated
branches for Axes 1/4; production files, callers, seams, and loaded skills for
Axes 2/3; plus any missing optional observations).

## Complementary Panel

When [Review Trigger](../shared/review.md#trigger) selects a 2-lens panel, run
**Lens A** and **Lens B** in parallel on the frozen candidate. `PASS` requires
both lenses `PASS`; keep any `FAIL` or `NEEDS_PARENT` verbatim per axis without
reranking, resolving an evidence-backed lens conflict via `adjudicator-agent`.

## Delta Recheck

Inspect the pinned repair delta (or updated candidate diff in `no-commit` mode),
repaired and open findings, any partial refutations alongside fixes, repair
decisions, and invalidated proof across all four axes: falsify root-cause
closure, preserved spec parity, clean repair structure (`Engineering` / `Rust
Change Surface`), and run the Mutation Mental Test on every new or edited
regression test. Keep unaffected closed reasoning intact. (Complete refutations
with zero code changes route directly to `adjudicator-agent`.)

## Structure-Only Review

Apply Axis 3 strictly to the structural additions named in the brief.
