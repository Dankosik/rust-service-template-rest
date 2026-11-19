# Technical Design Review

Use when shared [Review](../shared/review.md) routes one fixed Technical Design
candidate (`specs/<task>/design/design.md` at `status: draft`, any linked
`design/*.md` and `rollout.md`, or a no-open-decisions evidence result) or on
an explicit standalone design review. Run statically over repository code,
`Cargo.lock` crate sources (via `Cargo.lock` or read-only `cargo metadata
--locked --format-version 1`), and accepted specs—no builds, tests, or
services. Stay read-only: treat `design.md`'s prose and `Verified anchors` as
claims to falsify against primary sources, record every inspected primary
source (files, symbols, `Cargo.lock` crate sources) in `evidence_boundary`, and
return [Review Result V1](../interfaces/review-result-v1.md) without rewriting
`design/` or inventing decisions.

For a no-open-decisions result, jump directly to [Verdict](#verdict).

## Axes

- **Default (1 fresh `reviewer-agent`):** run all applicable falsifiers below.
- **Two-axis parallel review (2 fresh `reviewer-agent` lanes, non-overlapping
  partition):** run strictly the assigned axis when briefed for one; review
  passes only when **both** axes clear (`PASS` or `CONCERNS`) on the same
  candidate without masking findings across axes:
  - **Axis 1 — Spec Fidelity, System Mechanics & Rollout:** falsifiers **1, 4,
    and 5** (full scope).
  - **Axis 2 — Architecture, Source Truth & Planning Handoff:** falsifiers **2,
    3, and 6** (full scope).

## Adversarial Falsifiers

Stop a broken path at its first unsupported edge while continuing independent
paths so one pass surfaces all material defects. Give every scenario finding a
concrete witness (trigger, broken edge, and exact anchor in `spec.md`,
`design/`, or primary source).

### Tier 1 — Fidelity, Simplicity, Architecture & Primary-Source Truth

*If Tier 1 exposes a missing obligation, invalid mechanism, or
architecture/compile violation, skip Tier 2 cold-start polishing on malformed
sections while still reporting upstream gaps found in the same pass.*

1. **Obligation trace, spec fidelity & scope.** Starting independently from
   `spec.md` (`R<n>`, `G<n>`, `Terminology`, `Non-goals`, `Decisions,
   assumptions, and reopen triggers`—including `Technical Design` `Downstream
   handoff` and `Durable contract update`—and `Carried concerns`) and canonical
   contracts, reconstruct `obligation -> mechanism/decision -> semantic owner
   -> contract/truth -> proving surface`. `FAIL` on any unsupported obligation,
   tripped `spec.md` `Reopen if` trigger, contradicted/altered `spec.md`
   behavior, or leaked `Non-goal`.
2. **Simplicity, deletion test & steelman `D<n>`.** Try to delete or collapse
   every added component, crate, module, trait, dependency, queue, fallback, or
   config key onto an existing owner, `std`, or a locked crate (unless forced
   by an explicit `spec.md` obligation). `FAIL` on speculative generality,
   shallow pass-through chains, single-adapter mock-only traits, unjustified
   custom infrastructure, or any `D<n>` backed by a strawman alternative or
   missing decisive constraint / objective `Reopen if`.
3. **Primary-source truth, workspace rules & compile reality.** Independently
   check real code, `Cargo.lock` crate sources (**signatures and semantics**,
   not mere existence),
   [`quality/architecture.json`](../../../quality/architecture.json)
   (`allow_dependents` included), `scripts/template/profiles.json`, workspace
   `Cargo.toml` lints (`unwrap_used`, `expect_used`, `panic`, `unsafe_code`),
   `clippy.toml` disallowed methods/types, patched `vendor/` forks,
   `docs/decisions/*.md`, and [Backend Library
   Selection](../../backend-library-selection.md). `FAIL` on:
   - any symbol, config key, schema, or locked crate API contradicting source
     or claimed verified without checking a cached source (only genuinely
     uncached crates carried in `Reopen if` qualify as `CONCERNS`);
   - any edge violating `quality/architecture.json`, missing
     `allow_dependents`/`profiles.json` registration, or coupling optional
     profiles;
   - reversing or adopting a rejected `docs/decisions/` or
     `docs/backend-library-selection.md` choice without meeting its reopen
     condition or flagging the required durable doc update;
   - any Rust seam or caller sketch failing static compile reality
     (`Send + Sync + 'static` across `.await`, object safety, `&mut Tx<'_>` /
     `OperationContext` lifetimes, `axum` / `sqlx` / `utoipa`, `Cargo.toml`
     lints, `clippy.toml` bans) or forcing `pub` internal leaks to test.

### Tier 2 — Failure, Trust, Budget, Rollout & Planning Cold-Start

4. **Failure, data/tx, trust, cancellation, race & budget attacks** *(System
   Design).* Attack every real crossing with concrete witnesses and `FAIL` on
   any unclosed gap or abstract prose leaving the mechanism undecided:
   - *Data & tx:* DB tx/connection held across network I/O, broken atomic
     commit scope, unbounded scan/growth, pruning breaking replay/dedup, or
     cross-tenant cache key collision;
   - *Trust:* AuthN/AuthZ bypass via another entry point (gRPC, worker,
     redrive), secret/PII leak in `infra_http::Problem`/`tonic::Status`/
     `service_failure::Code` details/`Debug`/logs/metrics, unchecked
     egress/SSRF, or untrusted parser amplification;
   - *`CommitUnknown` & recovery:* timeout/disconnect/crash where retries
     duplicate or drop effects, or missing in-flight/committed/expired
     idempotency rules;
   - *Cancellation & races:* dropped future or drain leaking locks, txs,
     permits, or tasks; TOCTOU, double-apply, stampede, or smallest breaking
     interleaving across interacting flows;
   - *Budgets, cost & signals:* downstream timeout + retry sum exceeding caller
     deadline (`OperationContext`) or worker drain exceeding bootstrap shutdown
     budget (`crates/service`, `crates/jobs-worker`), unbounded unit cost, or
     non-success path lacking a distinct operator signal or readiness rule.
5. **Rollout, mixed-version & required-input closure** *(deployable, worker,
   contract, schema, config, trust material, or managed dependency changes).*
   Test rolling-deploy mixed-version overlap, lock-safe DDL, rollback/roll-
   forward safety, external input/authority ownership, and [Release
   Closure](../rubrics/release-closure.md) (`rollout.md` when non-trivial).
   `FAIL` if rollout breaks live callers/workers or leaves required inputs
   unowned (deploy-only permissions stay at deployment).
6. **Planning cold-start, pinned identifiers & boundary discipline.** Walk
   `intent.md`, `spec.md`, `design/design.md` (and `rollout.md`), and cited
   code with **zero chat history** (**two-Leads divergence probe**). `FAIL` if
   Planning or parallel Leads must invent a mechanism, failure rule, owner, or
   shared identifier (config key, error code, metric/span name,
   wire/event/job/column name); if `Shared mutated surfaces` omits a shared
   lock path or uses prose instead of canonical repo-relative paths; if
   `Planning hooks` suggests a `shared seam` with `<2` parallel consumers; or
   if `design/` prescribes task slicing, function bodies, brittle private `.rs`
   file trees, or test cases/fixtures/proving layers/commands/unmandated live
   gates ([Evidence Contract](../shared/evidence-contract.md#design-proof)).

## Verdict

- **Draft design:**
  - `PASS`: every applicable falsifier clears with primary-source inspection
    recorded in `evidence_boundary` and zero downstream invention. Never block
    `PASS` on local choices behind a seam, private file splits, Planning task
    slicing, Implementation test authoring, deploy-only permissions, or
    equivalent valid designs preserving ownership, invariants, and bounds.
  - `CONCERNS`: only a bounded later proof/risk over an otherwise closed design
    (e.g. genuinely uncached `Cargo.lock` crate source or optional benchmark);
    name risk, owner, and consuming action for `Carried concerns`.
  - `FAIL`: any surviving falsifier anchored to `R<n>`/`G<n>`, `D<n>`, or
    primary source with smallest owner (`Technical Design`, `Specification`,
    `Intake`, or external authority).
- **No-open-decisions result:**
  - `PASS`: primary-source evidence recorded in `evidence_boundary` proves
    every `R<n>`, `G<n>`, `Technical Design` `Downstream handoff`, and
    `spec.md` `Carried concerns` item is mechanically forced without
    architectural ambiguity.
  - `CONCERNS`: every choice is mechanically forced, with only a bounded later
    proof or risk obligation (such as a genuinely uncached `Cargo.lock` source
    check or an unresolved downstream proof concern carried from `spec.md`);
    name risk, owner, and consuming action so Technical Design carries it in
    its inline `Transition Result V1`.
  - `FAIL`: any open architectural choice or source contradiction survives
    (`reopen_owner: Technical Design`, `Specification`, `Intake`, or external
    authority).
