# T1 — Bounded and recoverable jobs

Outcome:
Deliver complete B1–B6 reliability: bounded attempt/completion custody, failed
custody until explicit resolution, safe PostgreSQL-only recovery, honest process
observation and immutable outbox identity/history. Retain retry/lease/process limits.

Consumes:

- [Intent](../intent.md) and [Specification](../spec.md) — unchanged accepted outcome and authority.
- [Ready adopted Design](../technical-design-transition.md), [system](../design/system.md), [ownership](../design/ownership.md), [rollout](../design/rollout.md) — immutable foundation and remaining D1–D6, independently reviewed PASS.
- Immutable foundation `5a683be7098fdba4981afffd774f59fed40145ed`, fast-forwarded by root into this isolated branch — common code/tests/migrations/metadata/profiles. Never consume parallel-checkout dirt.
- [Implementation](../../../docs/spec-first-workflow/phases/implementation.md) and current repository/configuration/persistence/validation owners — executor-owned tests and one assembled final-validation boundary.

Provides:
Complete corrected deliverable, necessary regression coverage, generated/profile
closure, canonical guidance and an Implemented handoff after writers join.

Boundary:
The original reviewed T1 unit and acceptance boundary remain unchanged. This
refresh adopts available implementation after the reviewed Design reopen; do
not reimplement covered core or create per-lane proof gates.

| Obligation | Reused implementation | Remaining correction |
| --- | --- | --- |
| B1 | Shared permits, cancellation cleanup and earliest deadlines | D1: retire all batch entries/custody and SQL buffers before any reply, including cancellation-induced sender closure. |
| B2/B3 | Completed-only retention, archived history, locked fenced recovery, conflict/unknown outcomes and bounded pages | D3/D4: unconditional canonical writable admission and 2s local statement timeout before recovery lock. Keep actual existing 12s backstop. |
| B4 | Same job/outbox identity, payload, trace and previous cycles | D6: indefinite replay versus finite dedup TTL, saved-command invalidation after restore and effect reconciliation. |
| B5 | Sole union sampler, failed depth and safe traversal | D2: late-series initialization, timestamp invalidation and captured-union comparison under peer mutex. D5: JSON schema version and declared normalized handled set, including failure results. |
| B6 | Existing policies and basic guides | D6: retry arithmetic/caveats and poison/rolling-kind/payload compatibility. |
| Compatibility | Existing schema, metadata, CLI/config/profile containment | Necessary mechanical compiler/fixture repairs and changed consumers only. |

Adopt recovery_history archives, caller-owned provisional Tx API, RFC3339
microseconds, 500-row/1024-kind bounds and existing comma-list CLI. The prior
recovery_generation representation/pool API are superseded. No new dependency,
migration, runtime, SLO or whole-core rewrite is selected.

Mutable owners:
The Lead owns targeted implementation and serial integration. Optional lanes
must use disjoint files and release them before integration:

- Custody/observation: infra-jobs attempt.rs, engine.rs, maintenance.rs, their local tests and applicable existing jobs integration cases.
- Operator: infra-jobs operator.rs; jobs-worker operator.rs/lib.rs and existing operator/process coverage. Coordinate maintenance signature changes with its sole writer.
- Guidance: existing jobs/async/outbox/rollout/configuration guides and matching inventory only if changed source requires it.
- Lead: packet check locators, serial integration and genuine SQLx regeneration only when fixed SQL changes.

Root alone writes tasks.md and workflow-plan.md. Preserve unrelated work and
never change the parallel checkout/branch. User authorized edits/checks/commit,
push and one PR, not merge/deploy/live recovery. Messaging the parallel chat
still needs the pending explicit permission; local preparation continues.

Exclusive locks:
Delegated write scopes; Lead-only SQLx directory after query/schema writers
freeze/join; serialized CPU-heavy validation under the shared Git-common lock;
root-only ledger. No new migration/manifest mutation is selected. Any real
in-scope need follows its owner and scheduling before mutation.

Final validation:

- Claim: complete B1–B6 and D1–D6 deliverable has no surviving in-scope defect.
- Checks: matching build/workspace tests for the original manifest/multi-crate change, docs checks and selected existing database/migration/SQLx/outbox/profile evidence through local/CI owners. Concrete tests/commands are executor-owned. Reuse proof only for unchanged inputs; no duplicate heavy matrix/new runner.
- Observable: unit/process behavior, real PostgreSQL for claimed database semantics, existing outbox/profile identity/containment proof. PR presence and baseline passes do not prove this candidate. One assembled final independent review follows implementation.
- Delivery: commit/push and one reviewable PR with selected CI results; root reconciles overlapping PR ownership. No merge/deploy/live queue effects.

Reopen if:
Changed behavior goes to Specification; mechanism/rollout to System Design;
responsibility/dependency/generated authority to Rust Ownership; requester
meaning/authority to Intake. Routine naming, fixtures, generation, compiler
feedback and repairs remain Implementation work. This input refresh preserves
all final Completion requirements and adds no task/review gate.

Concrete implementation locators:

- D1: `crates/infra-jobs/src/attempt.rs` —
  `entire_batch_retires_before_success_failure_cancellation_or_pruning_wakes_a_waiter`
  covers multiple entries, unchanged/success/failure, cancellation and pruning.
- D2: `crates/infra-jobs/src/engine.rs` —
  `late_peer_invalidates_freshness_and_rejects_an_in_flight_old_union` covers
  new-series initialization, retained previous values, invalidation and refusal
  to publish a superseded union.
- D3: `test/tests/jobs/operator.rs` —
  `worker_operator_refuses_read_only_sessions_for_every_command`.
- D4: `test/tests/jobs/operator.rs` —
  `recovery_initial_lock_has_local_statement_budget_and_preserves_failed_custody`.
- D5: existing
  `worker_operator_commands_commit_with_postgres_only_and_emit_safe_receipts`
  and `operator_commands_use_only_postgres_configuration_and_safe_identity_receipts`
  cover schema version and normalized handled kinds on success and refusal.

These are authored cases, not executed proof or extra gates. The ordinary
`make build` / `make test` plan already covers crate-local and binary-process
cases. The existing selected PostgreSQL jobs target is
`bash scripts/ci/test-integration-db.sh --test jobs`; focused crate repair can
use `make test-package PKG=infra-jobs` or `PKG=jobs-worker`. Final delivery owns
command admission and reuse, including the pending integration with current main.

Implementation feedback: targeted rustfmt and `git diff --check` passed. A
serialized compile-only command,
`cargo check --locked -p infra-jobs -p jobs-worker -p integration-tests --all-targets --features integration`,
did not start: the shared validation lock remained owned by another worktree's
`make test-changed`, and the 15-second lock wait returned 75. No compiler or
behavioral result is claimed. Fixed SQL and schema are unchanged; the new
mutation use reuses the exact committed metadata for
`SET LOCAL statement_timeout = '2000ms'`
(`.sqlx/query-2a05fba9b58d02b7e5e03748e35a57e9bf31410850c479d6040f21606c0b7add.json`).
No source regeneration is required or was run for this delta.
