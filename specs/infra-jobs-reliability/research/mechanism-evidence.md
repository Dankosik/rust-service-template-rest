# Supporting mechanism evidence

Read-only evidence captured 2026-10-02 for [Technical Design](../design/system.md).
This supports its remaining extension-point decisions, not a new queue survey
or standalone Research phase. Current source baseline is
`67be869acea112af271ec8ba621cbc50ae9d36b7`.

| Mechanism | Exact evidence | Decision effect / limits |
| --- | --- | --- |
| Admission custody | Cargo.lock resolves Tokio 1.53.1. Its [OwnedSemaphorePermit](https://docs.rs/tokio/1.53.1/tokio/sync/struct.OwnedSemaphorePermit.html) owns permits through Drop; repository claim.rs already transfers it to supervise. attempt.rs has one queue mutex, one asynchronous writer mutex and oneshot replies. | Reuse standard Arc plus owned permits and scoped drop cleanup; no channel/lifecycle dependency. Tokio does not remove an application-owned Vec entry when a waiting future is cancelled, so that application bookkeeping remains local. |
| CLI parser | Cargo.lock resolves Clap 4.6.7, already used by service-config LoadOptions. Installed official crate source `clap-4.6.7/examples/tutorial_derive/03_04_subcommands.rs` demonstrates derive Subcommand; `_derive/mod.rs` and `_derive/_tutorial.rs` document flatten/optional subcommands. Exact-version docs.rs requests returned an error, so the installed resolved source is the evidence. | Add the existing workspace Clap dependency to jobs-worker; reuse LoadOptions by flattening. No parser replacement or version upgrade. |
| Configuration projection | config/load.rs merge<T> runs namespace and file pre-scans before DeserializeOwned projection; load_migration and postgres.rs MigrationConfig already ignore unused root sections while validating retained ones. | Add a jobs-specific PostgreSQL projection on the same seam. Do not create a second source precedence implementation or load provider secrets. |
| Recovery generation | The existing CLAIM draws nextval from background_jobs_claim_generation, a bigint sequence with default NO CYCLE. PostgreSQL [sequence semantics](https://www.postgresql.org/docs/18/functions-sequence.html) keep allocated values spent across rollback. | Reuse the sequence for redrive. It does not guarantee external monotonicity after backup restoration/manual reset; saved commands must be invalidated by that operational boundary. |
| Uniqueness | Existing background_jobs_live_unique_key is partial over pending/running kind and C-collated key. PostgreSQL [unique indexes](https://www.postgresql.org/docs/18/indexes-unique.html) enforce multicolumn equality. | Let the unique index settle enqueue/redrive races, classify only its named 23505, and let in_tx roll back. No preflight key query is authoritative. |
| Query work | PostgreSQL [WITH semantics](https://www.postgresql.org/docs/18/queries-with.html) show that query folding/placement matters. The selected implementation does not need custom planner control: fetch a fixed PK window first, then filter in Rust. | At most 1000 visible rows leave PostgreSQL per invocation. MVCC/index work is not constant; local statement/client budgets remain required. |
| Index rollout | PostgreSQL [CREATE INDEX](https://www.postgresql.org/docs/18/sql-createindex.html) supports concurrent partial indexes, requires concurrent builds outside a transaction, and warns about invalid indexes after failure. Repository migrations/README.md and migrate runner already own that recovery. | Append a separate concurrent failed-kind index migration, reuse the migrator's invalid-index refusal and deployment deadline. |
| Transactions | Cargo.lock resolves SQLx 0.9.0. infra-postgres/transaction.rs owns acquire/begin cancellation, explicit isolation, opaque Tx and CommitFailed/CommitUnknown classification; persistence architecture prohibits automatic generic retry. | Operator mutations use this boundary and remain one-row actions. No raw autocommit mutation with ambiguous result handling or new transaction helper. |

No version upgrade is proposed. Installed serde/serde_json/uuid and the
workspace Clap declaration already cover safe output and parsing. New direct
manifest edges must remain jobs-prunable and deliberate Cargo.lock changes
must be included if package dependency lists change. Implementation checks the
resolved feature graph and existing dependency gates for those changed edges.
The strongest alternative queue packages and their current exclusions were
already evaluated in the accepted Async Architecture on 2026-10-01; none
provides a reason to replace this accepted queue for a bounded correction.


## Narrow source adoption, 2026-10-02

The implementation at immutable `a04b699f744038bf9b529bdee89eda76c00f59a6`
(PR #225, core commit `ebb88eadce1b3cb30cde1686b8927e7a78d7bc95`) was read
through Git objects, not the active parallel worktree. It already supplies
permit custody/cancel-safe completion registration, failed custody, locked
same-identity recovery, safe PostgreSQL-only CLI, and the single process union
sampler. These are available source, not inherited proof for the assembled
candidate. Its descendant `5a683be7098fdba4981afffd774f59fed40145ed` differs
only in an equivalent `let else` in `infra-jobs/src/operator.rs::sqlstate` and
an existing webhook migration test's empty `recovery_history` projection.
Those verified mechanical repairs are admitted without reopening mechanisms.

| Evidence / closed fork | Adoption or remaining correction |
| --- | --- |
| operator.rs archives old version/attempts/failure/times/claimant/summary/errors before reset in the same fenced UPDATE. | Adopt same-row recovery_history and its two existing migrations; no recovery_generation column or failure-writer retagging. B3 requires distinguishable retained history, which either representation satisfies. |
| Operator API borrows opaque Tx; jobs-worker owns in_tx and emits success only after it acknowledges commit. | Adopt provisional API with explicit caller failure propagation and commit-finality documentation. No pool-wrapper API replacement. |
| Inspection uses fixed 500-row PK windows, 1024-name/66559-byte admission, v1 UUID cursor and UTC RFC3339 microseconds. CLI supplies --handled-kinds including explicit empty string. | Adopt bounded representation; Definition leaves exact bounds/encoding to Design. Add schema_version and validated handled-set echo from the existing owner. |
| engine.rs OPERATION_BACKSTOP is 12s at both original baseline and foundation; startup check is 5s. | Correct the former design's inaccurate existing-five-second provenance. Keep 12s, with 2s local read/mutation statements and the existing separate startup/close stages; no new total-duration SLO. |
| attempt.rs:589–643 builds SQL vectors and notifies popped entries while vectors/remaining batch entries survive. | D1 completes two-phase batch disposal before any reply wake, including cancellation closure. Existing one-entry wake proof does not establish that multi-entry property. |
| engine.rs:170–176 adds peers without invalidation/init; maintenance.rs:215 and 317–342 snapshots/publishes without membership recheck. | D2 closes late-peer freshness under the existing peer mutex. No second sampler or metric contract. |
| operator::inspect applies SET LOCAL 2000ms; lock_failed/redrive/discard do not. Startup admits inspection with require_writable false. | D3/D4 restore selected canonical-writer admission for all commands and apply the same 2s statement ceiling before the mutation lock. Keep history/secret admission and read-only read transactions. |
| Existing docs retain finite-horizon advice without complete indefinite-redrive/restore-token/poison wording. | D6 adds accepted B4/B6 guidance and preserves old-owner stop/upgrade/rollback limits. |

The available implementation eliminates the fork between hypothetical API/file
shapes. Its config, CLI, metadata and profile inventory are reused; subsequent
SQL changes still require genuine make sqlx-prepare output. The existing review
is retained only for unchanged semantic scope; a focused fresh adoption/delta
review owns the changed representation, interface and bypass-closure decisions.
