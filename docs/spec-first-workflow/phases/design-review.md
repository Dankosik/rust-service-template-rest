# Design Review

Fresh, static, read-only adversarial review of `specs/<task>/spec.md` and
`specs/<task>/design.md` (plus `intent.md` and `research/*.md` when present)
**before** [Planning](planning.md) slices tasks. Return [Review
Result](../shared/review.md#review-result) under [Review
Lifecycle](../shared/review.md#lifecycle).

## Topology And Brief Hygiene

- **2-axis parallel panel** when the design touches both runtime
  crossings/rollout and Rust seams across `≥2` crates, closes a **one-way
  door**, or carries non-trivial [Release
  Closure](technical-design/release-closure.md) gates.
- **Single reviewer** for focused designs (runs all applicable checks in one
  fresh context).
- Start every reviewer with a **fresh context window** (no parent chat history,
  author rationale, or self-review claims). Inspect primary sources statically
  (no builds, tests, or live services). Every assigned axis must clear
  independently on the same fixed bundle; return `REVIEW_HANDOFF_INVALID` if
  required artifacts (`spec.md`, `design.md`) are missing or unreadable.
- On a bounded **Delta Recheck** after local repairs that preserve interfaces
  and core mechanisms, inspect only edited clauses, refutations, and directly
  dependent anchors; escalate to a fresh full review whenever repairs change
  interfaces or reopen a one-way door.

## Checks

Keep findings compact (**target `≤400` words per axis**) under explicit check
headings, recording inspected primary files, symbols, and `Cargo.lock` sources
in `evidence_boundary`:

```text
## Check <N> — <Check Name>
- none — attacked <strongest loophole, crossing, or anchor>; closed by <clause or locator>
# OR per finding:
- [<PHASE_DEFECT | UPSTREAM_GAP | CONCERN>] <anchor> — violated: <rule>; witness: <concrete failure or counterexample>; owner: <smallest owner>
```

Repo standards in `AGENTS.md`, `docs/repo-architecture.md`, `docs/decisions/`,
and `docs/backend-library-selection.md` override generic taste.

### Axis 1 — Behavior, System Crossings & Rollout

1. **Spec fidelity & behavioral loopholes:**
   - Verify every `R<n>`, `G<n>`, and non-goal maps to a real technical owner
     without altering confirmed behavior or leaking non-goals.
   - Attack `spec.md` for malicious compliance, external two-implementation
     divergence, tautological falsifiers, and missed [Behavioral
     Probes](specification/behavioral-probes.md).
2. **System mechanisms, reuse & crossing probes:**
   - Apply the **deletion test** to every added system mechanism, queue, cache,
     worker, fallback, or config key; `FAIL` when an existing `crates/infra-*`
     owner, `docs/decisions/` pattern, or simpler substitute satisfies every
     `R<n>`, `G<n>`, and bound.
   - Attack every exposed runtime crossing against [System Crossings &
     Probes](technical-design/system-crossings.md), verifying cited configs,
     budgets, middleware, and migrations in primary code. `FAIL` on abstract
     reassurances lacking concrete constraints, budget arithmetic, or 4-step
     interleaving witnesses, or on any unclosed `CommitUnknown`, cancel-safety,
     trust, or stacked-retry defect.
3. **Rollout & mixed-version safety:**
   - Verify deployment impact classification, [Release
     Closure](technical-design/release-closure.md) gates, lock-safe DDL,
     forward/backward payload compatibility across `migrate` / `service` /
     `jobs-worker`, and `same-release` vs `multi-release` boundaries.

### Axis 2 — Rust Architecture, Compile Reality & Handoff Readiness

4. **One-way doors, deep seams & deletion test** *(audit against [Rust
   Architecture](technical-design/rust-architecture.md) and [Design It
   Twice](technical-design/design-it-twice.md))*:
   - Verify every unfixed **one-way door** carries a `D<n>` backed by a real
     steelman and objective `Reopen if`—`FAIL` if a one-way door was smuggled
     in as "Forced" without a binding primary-source rule, or if a rejected
     `docs/decisions/` or `docs/backend-library-selection.md` choice was
     reversed without its reopen condition.
   - Apply the **deletion test** and [Dependency
     Categories](technical-design/rust-architecture.md#dependency-categories)
     to every added crate, module, seam, trait, wrapper, dependency, or helper:
     `FAIL` on shallow pass-through chains, single-adapter mock-only traits,
     fake-adapter port traits wrapping local workspace backends, `Clock`/`Rng`
     traits, or custom helpers already covered by
     [workspace utility crates](../../../.agents/skills/rust-dependencies/references/workspace-crates.md).
   - Walk every `S<n>` **caller walkthrough** on both happy and worst-case
     failure/cancel paths (`FAIL` on magic call order, `&mut Tx<'_>` across
     network `.await`, or leaked internal errors). Verify required `Durable doc
     updates`.
5. **Compile reality & workspace constraints:**
   - Verify cited workspace types, callers, `Cargo.lock` APIs,
     [`quality/architecture.json`](../../../quality/architecture.json)
     (`roles.<role>.allow_roles`, `allow_members`, `allow_dependents`),
     `template/profiles.json`, `Cargo.toml` `[workspace.lints]`, `clippy.toml`,
     `vendor/` patches, and modified exhaustive-type fan-out
     (`service_failure::Code`, `AppState`, or public `service_config` enums)
     against real code.
   - `FAIL` on contradicted or uninspected anchors, unverified tricky bounds
     (`RPITIT !Send`, `tower::Layer`, `FromRequest`, `utoipa`, `sqlx::query!`)
     not honestly flagged as `A<n>` for `[integration-first]`, or prescribed
     internal `.rs` trees, function bodies, or test commands.
6. **Planning handoff completeness:**
   - Verify parallel Leads will not have to invent shared config keys, error
     codes, metric names, wire/table names, or lock surfaces.
   - Confirm superseded code/tests and generated contracts (`api/`, `.sqlx/`,
     `crates/grpc-contracts`) are accounted for.

## Verdict

- `PASS`: every assigned check clears with proof of attack and primary-source
  inspection. Repo-endorsed patterns, behind-seam implementation choices, and
  speculative out-of-scope scenarios never block `PASS`.
- `CONCERNS`: strictly a bounded downstream proof risk (for example, an
  uncached crate check or an explicitly flagged `[integration-first]` compile
  or integration assumption) over an otherwise closed design, naming risk,
  owner, and closing action.
- `FAIL`: any surviving witnessed `PHASE_DEFECT` (repair locally) or
  `UPSTREAM_GAP` (reopen the user strictly for a confirmed product/risk
  contradiction or missing external authority).
