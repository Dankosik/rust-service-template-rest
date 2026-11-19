# Implementation Review

Use when shared [Review](../shared/review.md) routes a fixed implementation
candidate (assembled ledger, single-unit delivery, or standalone review). Stay
read-only and review directly without repairing, re-scoping, or accepting.

Pin the candidate diff (`<Base>..HEAD` or supplied diff command), confirm the
ref resolves and the diff is non-empty (`REVIEW_HANDOFF_INVALID` otherwise),
and **read touched production code — including surrounding functions, modules,
and direct callers — before its tests** (never judge from diff hunks alone).
Consume accepted sources (`intent.md`, `spec.md`, `design/`, `rollout.md`,
`tasks.md`, packets) and check receipts; run a bounded existing diagnostic only
for a concrete falsification question under [Evidence
Contract](../shared/evidence-contract.md#required-and-optional-proof).

## Adversarial Falsifiers

Evaluate every assigned axis (all four in single-reviewer mode, or your pair in
[Complementary Panel](#complementary-panel)) under explicit per-axis headings
so one passing axis never masks another's defect. Skip anything deterministic
tooling (`rustc`, `clippy` — including `unwrap`/`expect`/`panic!`/`todo!`/`dbg!`,
`rustfmt`, `quality/architecture.json`, `jscpd`) already enforces.

1. **Outcome, Spec & Scope Parity.** Start from accepted sources and
   `Completion`. Construct a concrete caller or operator scenario where every
   executed test stays green yet an accepted obligation, preserved `G<n>`,
   withdrawn removal path, or `Completion` criterion fails. Trace every
   material hunk back to an accepted obligation; flag unrequested behavior or
   silent scope drift.
2. **Real-Path Defects & Cross-Unit Seams.** Attack touched production code
   with five probes:
   - *Precedence & rejection:* colliding rules or errors; state mutation or
     info leak before rejection.
   - *Partial failure & cancellation:* future dropped mid-`.await`, halfway
     write failure, timeout, held lock/permit, non-idempotent retry, or
     irreversible migration.
   - *Silent degradation:* `let _ =`, `.ok()`, catch-all arms, or defaults
     turning real failures into silent success.
   - *Resource bounds & privacy:* unbounded untrusted allocation, batch size,
     or task spawning; high-cardinality metric labels; or secret/PII leakage
     into logs, errors, or telemetry.
   - *Cross-unit seams & sibling drift:* compare exact types, error codes,
     units, defaults, and recorded `decisions` across `Provides -> Depends on`
     seams (`INTEGRATION_DEFECT`), and check parallel sibling tasks for
     duplicated private helpers or divergent vocabulary, error shapes, or
     telemetry fields.
3. **Structure & Single Ownership.** Apply `rust-structural-quality`'s
   **deletion test**, [Engineering](../../../AGENTS.md#engineering), and [Rust
   Change Surface](../../../AGENTS.md#rust-change-surface). Flag reimplementing
   `std`, a locked `Cargo.lock` crate, or an authoritative tool; splitting one
   fact across hand-synced carriers; adding an unjustified crate,
   dependency/feature, layer, trait, config key, or fallback/shim; or violating
   non-clippy repo rules (explanatory comments, `#[non_exhaustive]` on public
   workspace types, temporary debug probes/logging, or unjustified policy
   waivers). **Repo standards override generic taste:** suppress objections
   endorsed by `AGENTS.md` or `docs/decisions/`. Name the violated constraint
   and exact item to delete, collapse, bind, or delegate.
4. **Test & Proof Falsifiability.** Apply the **Mutation Mental Test** to every
   added or edited test: mentally invert or delete the single production branch
   it claims to guard — would it go **red**? Flag **tautological** assertions
   (recomputing expected values like the code), mocking the claimed boundary,
   `is_ok()`-only or internal-step assertions, sleeps/retries masking flakes,
   duplicate same-layer suites, and deleted or weakened safety, durability, or
   sole regression guards.

## Blocking Bar And Output Format

**Verify reachability before blocking:** confirm in live callers and callees
that any blocking counterexample is reachable (not blocked by an upstream type
invariant, validator, or middleware). **A clean `PASS` with zero blocking
findings is the desired outcome when no reachable counterexample survives;
never invent speculative findings to look thorough.**

Return [Review Result V1](../interfaces/review-result-v1.md) (`PASS | FAIL |
NEEDS_PARENT`, or `REVIEW_HANDOFF_INVALID`), formatting `findings` compactly by
axis (no essays):

```text
## Axis <N> — <Axis Name>
- none — attacked: <one line naming concrete scenario or probe checked>
# OR per finding:
- [<TASK_DEFECT | INTEGRATION_DEFECT | UPSTREAM_GAP | FOLLOW_UP>] <path::Symbol> — violated: <cited claim/rule>; trace: <reachable counterexample/check>; owner: <smallest owner>
```

Blocking findings (`TASK_DEFECT`, `INTEGRATION_DEFECT`, `UPSTREAM_GAP`, and
real in-scope defects from optional checks) require all four fields above.
Style preferences, equivalent single-owner structure, naming polish,
speculative risks, untouched historical debt, and missing optional
infrastructure remain `FOLLOW_UP` and never block `PASS`.

## Complementary Panel

When [Review Trigger](../shared/review.md#trigger) selects a 2-lens panel, two
fresh `reviewer-agent` actors run in parallel on the frozen candidate:

- **Lens A — Spec, Scope & Proof:** Falsifiers **1 and 4** exclusively.
- **Lens B — Runtime Defects, Seams & Structure:** Falsifiers **2 and 3**
  exclusively.

**Synthesis (Delivery Owner):** `PASS` requires both lenses `PASS`; any `FAIL`
or `NEEDS_PARENT` holds verbatim per axis without reranking. Resolve an
evidence-backed lens conflict via `adjudicator-agent`. Post-repair recheck uses
one fresh [Delta Recheck](#delta-recheck) reviewer.

## Delta Recheck

Runs in **one fresh `reviewer-agent`** scoped to the repair diff
(`<old-HEAD>..<new-HEAD>`, or `git diff <Base> -- <repaired paths>` in
`no-commit` / inline mode), the brief's anchored findings, repair `decisions`,
and invalidated proof (covering all four axes on the repair delta, including
after a 2-lens Panel): falsify (1) root-cause closure of each finding,
(2) preserved spec parity and clean repair structure (`Engineering` and `Rust
Change Surface`), and (3) **Mutation Mental Test** quality on new or edited
regression tests. Keep unaffected closed reasoning intact.

## Structure-Only Review

Run **Falsifier 3** exclusively on each structural addition named in the brief;
keep behavior and proof outside scope. Classify
[Engineering](../../../AGENTS.md#engineering) and [Rust Change
Surface](../../../AGENTS.md#rust-change-surface) violations as `TASK_DEFECT`
naming the item to delete, collapse, bind, or delegate; classify other
observations as `FOLLOW_UP`.
