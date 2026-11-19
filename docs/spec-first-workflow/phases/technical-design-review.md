# Technical Design Review

Use when shared [Review](../shared/review.md) routes one fixed Technical Design
candidate (`specs/<task>/design/design.md` at `status: draft`, any linked
`design/*.md` and `rollout.md`, or a no-open-decisions evidence result) or on
an explicit standalone design review. Run statically over repository code,
`Cargo.lock` crate sources, and accepted specs—no builds, tests, or services.
Stay read-only: falsify claims independently against primary sources and return
anchored findings with their smallest owner in [Review Result
V1](../interfaces/review-result-v1.md) without rewriting `design/` or inventing
decisions.

For a no-open-decisions result, jump directly to [Verdict](#verdict).

## Review Topology

Choose single-reviewer or two-axis parallel review from the fixed candidate:

- **Default (1 fresh `reviewer-agent`):** run all applicable falsifiers below
  when only one branch triggered (System-only or Rust-Architecture-only) or the
  design is small and local.
- **Two-axis parallel review (2 fresh `reviewer-agent` lanes in parallel, never
  forking parent history):** use when *both* System Design and Rust
  Architecture are material, several crates or runtime boundaries change, or a
  hard-to-reverse fork or restructure is in play. Partition the fixed candidate
  so neither axis masks the other:
  - **Axis 1 — System, Flows, Failure & Operations:** runs falsifiers **1, 2
    (system additions), 3, and 4**.
  - **Axis 2 — Rust Architecture, Seams & Codebase Truth:** runs falsifiers **1
    (ownership trace), 2 (structural additions), 5, and 6**.

In two-axis review, the Technical Design owner keeps both reports distinct
without masking findings across axes; the review passes only when **both** axes
clear on the same candidate. After repair, re-run only the failed or
invalidated axis under shared [Review](../shared/review.md)'s delta and
freshness rules, retaining an unaffected axis's `PASS` when its scope is
unchanged. For one surviving evidence-backed conflict between axes, dispatch
`adjudicator-agent` under [Review](../shared/review.md#route).

## Adversarial Falsifiers

Run every falsifier applicable to the assigned axis and triggered branches.
Stop a broken path at its first unsupported or incompatible edge while
continuing independent paths so one pass surfaces all material defects. For
scenario-based findings, provide the concrete witness (trigger, broken edge or
failure path, and exact anchor in `spec.md`, `design/`, or primary source):

1. **Obligation trace & spec fidelity.** Start independently from `spec.md`
   (`R<n>`, `G<n>`, `Terminology`, `Carried concerns`) and affected canonical
   contracts. Reconstruct each obligation's trace through `design/`:
   `obligation -> mechanism/decision -> flow/finality -> contract/truth ->
   semantic owner -> required input -> proving surface`. `FAIL` if any
   obligation lacks a supported trace, if `design/` contradicts or silently
   alters `spec.md` behavior, or if unrequested behavior leaked into the
   design.
2. **Simplicity, deletion test & alternative rigor.** Trace every added
   component, crate, module, trait, dependency, queue, fallback, or config key
   back to the accepted obligation or constraint that forces it. Try to delete
   or collapse each addition—or replace a custom mechanism with an existing
   workspace owner, `std`, or a locked crate. `FAIL` on speculative generality,
   shallow pass-through layers, hypothetical single-adapter seams (mock-only
   traits), unjustified custom infrastructure, or non-trivial decisions (`D<n>`)
   lacking a genuine strongest rejected alternative, decisive constraint, or
   objective `Reopen if` trigger.
3. **Failure, cancellation, race, cost & operator-signal attacks** *(when
   System Design applies).* Attack every real crossing in the design:
   - construct a `CommitUnknown`, timeout, or partial-failure scenario where
     retries duplicate or drop effects;
   - drop the request future or trigger shutdown mid-flight to expose leaked
     locks, torn state, or orphaned background work;
   - run concurrent callers or workers against shared state to expose TOCTOU or
     unserialized races, and interleave interacting flows with the smallest
     ordering, interruption, or replay that could break a shared guarantee;
   - test caller-controlled input against the stated unit cost (round trips,
     queries, held locks, buffered bytes) to expose unbounded amplification;
   - verify each failure, degradation, and recovery path has a distinct
     operator signal and sound readiness/drain participation.
   `FAIL` on any unclosed failure, cancellation, concurrency, resource-bound,
   or observability gap.
4. **Rollout, mixed-version & required-input closure** *(when deployable,
   contract, schema, config, trust material, or managed dependency changes).*
   Test rolling-deploy mixed-version overlap (old and new replicas running
   together), rollback/roll-forward safety, external input/authority
   availability, and [Release Closure](../rubrics/release-closure.md) gates
   (`rollout.md` when non-trivial). `FAIL` if migration or rollout breaks live
   callers or leaves required inputs unowned (permission needed only to deploy
   belongs at deployment, not as a design blocker).
5. **Source truth, architecture rules & compile reality** *(when Rust
   Architecture or codebase anchors apply).* Independently inspect actual
   current code, `Cargo.lock` sources for cited crate APIs,
   [`quality/architecture.json`](../../../quality/architecture.json), optional
   template profile boundaries, `docs/decisions/*.md`, and [Backend Library
   Selection](../../backend-library-selection.md). `FAIL` if:
   - any cited symbol, config key, schema, or locked crate API contradicts real
     source;
   - any dependency edge violates `quality/architecture.json` or couples
     independent optional template profiles;
   - the design reverses or adopts a rejected choice from `docs/decisions/` or
     `docs/backend-library-selection.md` without meeting its stated reopen
     condition or flagging the required durable doc update;
   - any proposed Rust seam fails static compile reality (`Send + Sync +
     'static` across `.await`, object safety, `&mut Tx<'_>` /
     `OperationContext` lifetimes, `axum` / `sqlx` / `utoipa` constraints) or
     violates deep-module / interface-as-test-surface discipline (forcing `pub`
     internal leaks to test).
6. **Planning cold-start & boundary discipline.** Walk `intent.md`, `spec.md`,
   `design/design.md` (and `rollout.md`), and cited code with zero chat
   history. `FAIL` if a fresh Planning session must invent a mechanism,
   contract, failure rule, or crate/module owner; if `Shared mutated surfaces`
   omits an actual shared lock surface (`Cargo.toml`, migrations, OpenAPI,
   proto, `quality/architecture.json`, bootstrap, config); or if `design/`
   prescribes task slicing, function bodies, brittle private `.rs` file trees,
   or test cases, fixtures, proving layers, commands, or unmandated live gates
   ([Evidence Contract](../shared/evidence-contract.md#design-proof)).

## Verdict

- **Draft design:**
  - `PASS`: every applicable falsifier clears across all active axes without
    downstream invention. Never block `PASS` on routine implementation choices,
    private file splits, task slicing owned by Planning, test authoring owned
    by Implementation, deploy-only permissions, or stylistic preferences that
    preserve ownership, invariants, and bounds.
  - `CONCERNS`: only a bounded later proof or risk obligation over an otherwise
    closed design (such as an uncached locked crate source check or optional
    benchmark). Name the risk, owner, and consuming action so Technical Design
    persists it in `Carried concerns` before setting `status: ready`.
  - `FAIL`: any surviving falsifier; anchor each finding to its exact
    obligation (`R<n>`/`G<n>`), decision (`D<n>`), or primary source and name
    the smallest repair or upstream reopen owner (`Technical Design`,
    `Specification`, `Intake`, or external authority).
- **No-open-decisions result:** `PASS` requires primary-source evidence proving
  every `R<n>`, `G<n>`, and carried concern in `spec.md` is mechanically forced
  by current code, `quality/architecture.json`, and accepted contracts without
  architectural ambiguity; otherwise `FAIL` with `reopen_owner: Technical
  Design` (or `Specification` when product behavior is open).
