# Technical Design Review

Use when shared [Review](../shared/review.md) routes one fixed Technical Design
candidate (`specs/<task>/design/design.md` at `status: draft`, linked
`design/*.md` and `rollout.md`, or inline forced-owner evidence) or on an
explicit standalone design review. Run statically over code, non-obvious
`Cargo.lock` crate sources (via `Cargo.lock` or `cargo metadata --locked
--format-version 1`), committed contracts (`api/`), and accepted specs—no
builds, tests, or services. Stay read-only: falsify `design.md` prose and
`Verified anchors` against **primary sources**, record every inspected primary
file, symbol, and `Cargo.lock` source in `evidence_boundary` on every active
axis, and return [Review Result V1](../interfaces/review-result-v1.md) without
rewriting `design/` or inventing decisions.

## Review Topology

- **No-open-decisions result (always 1 fresh `reviewer-agent`):** inspect
  primary code, `quality/architecture.json`, and non-obvious `Cargo.lock`
  sources for every `spec.md` obligation (`R<n>`, `G<n>`, `Technical Design`
  `Downstream handoff`, `Durable contract update`, `Carried concerns`). Try to
  construct **one realistic two-engineer divergence** (mechanism,
  error/retryability, config default/validation, rollout/mixed-version path,
  invariant owner, or seam) or **one primary-source contradiction**, verify
  handoff completeness (`Shared mutated surfaces`, `Superseded paths and
  generated contracts`, `Durable doc updates`), then jump to [Blocking Bar And
  Verdict](#blocking-bar-and-verdict).
- **Draft design — default (1 fresh `reviewer-agent`):** run Tier 1 then Tier 2
  across all six applicable falsifiers.
- **Draft design — two-axis parallel review (2 fresh `reviewer-agent` lanes,
  never forking parent history):** run strictly the assigned non-overlapping
  axis, verifying primary sources for every claim inside that axis; review
  passes only when **both** axes clear (`PASS` or `CONCERNS`) on the same
  candidate without masking findings:
  - **Axis 1 — Spec Fidelity, System Mechanics & Rollout:** falsifiers **1, 4,
    and 5** (verifying primary system configs, budgets, middleware, contracts,
    and rollout boundaries; leaving deletion/steelman challenges to Axis 2).
  - **Axis 2 — Architecture, Source/Compile Truth & Planning Handoff:**
    falsifiers **2, 3, and 6** (verifying primary Rust code, non-obvious
    `Cargo.lock` APIs, workspace graph/lints, `docs/decisions/`, compile
    reality, deletion/steelman rigor, and cold-start handoff).

## Adversarial Falsifiers

Report under **explicit per-falsifier headings** (when clear, write `none —
attacked <witness, symbol, or crossing>` so passing checks prove execution and
never mask defects). Keep each finding compact (`anchor`, `falsified claim or
constraint`, `concrete witness`, `smallest owner` — no essays). Stop a broken
path at its first unsupported edge while continuing independent paths. **Repo
standards override generic taste:** suppress objections where
[AGENTS.md](../../../AGENTS.md), [Repository
Architecture](../../repo-architecture.md), `docs/decisions/`, or [Backend
Library Selection](../../backend-library-selection.md) endorses the pattern.

### Tier 1 — Fidelity, Simplicity, Architecture & Primary-Source Truth

*If Tier 1 exposes a missing obligation, invalid mechanism, or
architecture/compile violation, skip Tier 2 cold-start polishing on malformed
sections while still reporting upstream gaps found in the same pass.*

1. **Obligation trace, spec fidelity & scope.** Verify `spec.md` is `status:
   ready`, then independently map `spec.md` (all obligations, `Terminology`,
   `Non-goals`, `Assumptions/Reopen if`) + committed contracts (`api/`) `->
   mechanism/decision -> semantic owner -> proving surface`. `FAIL` on any
   unsupported obligation, tripped `spec.md` `Reopen if`, altered `spec.md`
   behavior, unresolved terminology collision, residual `Blocked decisions` in
   a `draft`, or leaked `Non-goal`.
2. **Simplicity, deletion test & steelman `D<n>`.** Try to delete or collapse
   every added mechanism, crate, module, trait, dependency, queue, fallback, or
   config key onto an existing owner, `std`, or a `Cargo.lock` crate (unless
   forced by `spec.md`). `FAIL` on speculative generality, shallow pass-through
   chains, single-adapter mock-only traits, `Clock`/`Rng` traits replaceable by
   plain parameters, unjustified custom machinery, or any `D<n>` backed by a
   strawman substitute or missing decisive constraint / objective `Reopen if`.
3. **Primary-source truth, workspace rules & compile reality.** Inspect real
   code, non-obvious `Cargo.lock` crate sources (**signatures and semantics**),
   [`quality/architecture.json`](../../../quality/architecture.json)
   (`allow_dependents`), `scripts/template/profiles.json`, `Cargo.toml`
   `[workspace.lints]`, `clippy.toml`, `vendor/` forks, `docs/decisions/*.md`,
   and [Backend Library Selection](../../backend-library-selection.md). `FAIL`
   on:
   - any symbol, config key, schema, or locked crate API contradicting source
     or claimed verified without inspecting cached source (only genuinely
     uncached crates carried in `Reopen if` qualify as `CONCERNS`);
   - any edge violating `quality/architecture.json`, business logic misplaced
     outside `crates/<feature>`, missing `allow_dependents`/`profiles.json`
     registration, or coupled optional profiles;
   - reversing/adopting a rejected `docs/decisions/` or
     `docs/backend-library-selection.md` choice without meeting its reopen
     condition, or omitting a required `Durable doc updates` entry;
   - any `S<n>` signature or caller sketch violating `Cargo.toml` lints,
     `clippy.toml` bans, or static compile reality (`Send + Sync + 'static`
     across `.await`, object safety, `&mut Tx<'_>` / `OperationContext`
     lifetimes, `FromRequestParts` vs `FromRequest`, `sqlx::query!` / `utoipa`
     bounds) without `Reopen if` + `integration-first`, or forcing `pub`
     internal leaks to test.

### Tier 2 — Failure, Trust, Budget, Rollout & Planning Cold-Start

4. **System mechanics, failure, trust, concurrency & budget attacks** *(System
   Design).* Attack every real crossing with concrete witnesses—verifying cited
   system configs, budgets, and middleware in primary code—and `FAIL` on any
   unclosed gap or undecided mechanism:
   - *Data & tx:* DB tx/connection held across network I/O, broken atomic
     scope, unbounded scan/growth, pruning breaking replay/dedup, or
     cross-tenant cache key collision;
   - *Failure, `CommitUnknown` & degradation:* conflicting/missing retryable vs
     terminal mapping, missing dependency degradation policy, or crash/timeout
     duplicating or dropping effects (or missing in-flight/committed/expired
     idempotency);
   - *Trust:* AuthN/AuthZ bypass via gRPC, worker, or redrive; secret/PII leak
     in `infra_http::Problem`/`tonic::Status`/`Debug`/logs/metrics; unchecked
     egress/SSRF or untrusted parser amplification;
   - *Cancellation & races:* dropped future, `tokio::select!` arm, or drain
     leaking locks/txs/permits/tasks; TOCTOU, double-apply, stampede, or a
     breaking interleaving across interacting flows;
   - *Budgets, backpressure, config & signals:* timeout + retry sum exceeding
     `OperationContext` deadline or drain exceeding bootstrap shutdown budget
     (`crates/service`, `crates/jobs-worker`); unbounded unit cost or missing
     concurrency/queue cap & shedding; new config missing units, defaults,
     startup validation, or inert-profile rule; high-cardinality metric labels,
     dropped async correlation, or indistinguishable failure/readiness signal.
5. **Rollout, mixed-version & required-input closure** *(deployable, worker,
   contract, schema, config, trust material, or managed dependency changes).*
   Attack rolling-deploy mixed-version overlap across `crates/migrate`,
   `crates/service`, and `crates/jobs-worker`—lock-safe DDL **and** serialized
   row/cache/job/event payload compatibility—plus rollback/roll-forward safety,
   external input ownership, and [Release
   Closure](../rubrics/release-closure.md) (`rollout.md` when non-trivial).
   `FAIL` if rollout breaks live callers/workers or leaves required inputs
   unowned.
6. **Planning cold-start, two-Leads divergence & boundary discipline.** Walk
   `intent.md`, `spec.md`, `design/design.md` (and `rollout.md`), and cited
   code with **zero chat history**. `FAIL` if Planning or parallel Leads must
   invent a mechanism, error/retryability rule, config default/validation,
   owner, or shared identifier; if `Shared mutated surfaces` omits a shared
   wiring file/lock path or uses prose instead of narrow canonical
   repo-relative paths; if `Superseded paths and generated contracts` or
   `Durable doc updates` drops a required cleanup, regeneration, or doc update;
   if `Planning hooks` suggests a `shared-contract` without `≥2` lock-disjoint
   parallel consumers; or if `design/` prescribes function bodies, brittle
   private `.rs` trees, task slicing, or test cases, fixtures, proving layers,
   commands, or unmandated live gates.

## Blocking Bar And Verdict

| Disposition | Bar |
| --- | --- |
| Blocking (`FAIL`) | Surviving falsifier anchored to `R<n>`/`G<n>`, `D<n>`/`S<n>`, or primary source with concrete witness and smallest owner (`Technical Design`, `Specification`, `Intake`, or external authority). |
| Bounded (`CONCERNS`) | Only a bounded later proof/risk over a closed design (e.g. genuinely uncached `Cargo.lock` source or optional benchmark); name **risk, downstream owner, and closing action** for `Carried concerns` (or inline `Transition Result V1`). |
| Non-blocking (never blocks `PASS`) | Local choices behind a seam, private file splits, Planning slicing, Implementation test authoring, deploy-only permissions, repo-endorsed patterns, or equivalent valid designs preserving ownership, invariants, and bounds. |

- **Draft design:** `PASS` when every applicable falsifier clears with
  primary-source inspection in `evidence_boundary` and zero downstream
  invention; `CONCERNS` or `FAIL` per table above.
- **No-open-decisions result:** `PASS` when primary-source evidence survives
  the No-Open-Decisions Falsifier with complete handoff surfaces; `CONCERNS` or
  `FAIL` per table above.
