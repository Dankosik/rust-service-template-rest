# Mechanism evidence for Technical Design

Status: ready

Scope: support [the design](../design/system.md), not standalone research or a
new queue-library selection. Inspected 2026-10-02 at source baseline
`67be869acea112af271ec8ba621cbc50ae9d36b7`. Evidence is source/API semantics;
this phase ran no benchmark, build or runtime test.

## Existing support and alternatives

| Need | Accepted reuse and current evidence | Strongest alternative, cost and reopen condition |
| --- | --- | --- |
| Keep admitted attempts and completion bookkeeping bounded | `attempt.rs::run_attempt`, `Completions`, `complete_batched`, `write_batch`; `claim.rs::dispatch_known`; existing owned semaphores, standard Arc/Drop, Tokio oneshot and one writer mutex | A bounded MPSC completer still needs in-flight ownership and cancellation cleanup and adds a worker task. Removing batching loses the supported group commit. Reopen on a concrete failure of this RAII contract or measured batch cost, not a different framework preference. |
| Non-reusable recovery fence | Canonical jobs migration already creates a bigint, non-cycling `background_jobs_claim_generation`; claim uses nextval and outcomes fence on id/generation | New UUID version or row-local counter adds another authority. Reuse the existing sequence, including its no-reset requirement and exhaustion failure. |
| Historical recovery cycles | Existing `errors` is per spent attempt; append previous cycle to a new `recovery_history` JSONB column during the same locked UPDATE | Tag every error writer with a new cycle field, or create an audit table. The former expands claim/rescue/outcome and mixed-version format work; the latter adds independent storage/retention without a permanent audit requirement. Reopen for history-byte cost or audit policy. |
| Operator CLI | Already-resolved clap 4.6.7 supports derive Parser/Args/Subcommand and flatten; `service_config::LoadOptions` derives Parser; no-subcommand keeps current worker entry | Manual argv split repeats parsing/help/error semantics. A separate binary or HTTP surface adds a lifecycle/delivery/exposure owner. Reopen only for a genuinely independent executable requirement. |
| Narrow config | Existing `load.rs::merge<T>` and `load_migration` already support a typed subset with shared source/secret scans; `JobsOperatorConfig` needs postgres only | Full Config admission incorrectly demands unrelated secrets, while duplicating environment/DSN parsing bypasses canonical policy. No new generic loader API is needed. |
| Transaction arbitration | `infra_postgres::in_tx`, opaque Tx, current CommitFailed/CommitUnknown rules and live-key partial unique index | Own transaction wrapper, savepoints or read-before-write key check add no guarantee. Propagate a unique violation so the shared boundary rolls back; reconcile unknown by identity. |
| Bounded unknown-kind traversal | Existing UUID primary key; fetch bounded safe rows, then filter with standard BTreeSet | SQL anti-join before LIMIT can read the entire known prefix; a global kind inventory creates refresh authority. The accepted traversal costs pages containing no matches. |
| Consistent failed visibility | `maintenance.rs` already caps per-kind counts at 1000, preserves last-good samples and publishes timestamp last; `Shared.peers` already collects every engine's registered names | Separate failed sampler or per-engine freshness timestamps create another sample contract. Extend the process-duty owner's one union sample. Reopen only for measured union cost. |

No new crate version or general infrastructure framework is selected. Reuse
the workspace's locked `sqlx 0.9.0`, `tokio 1.53.1`, `clap 4.6.7`, and
`serde_json 1.0.151`; only direct dependency edges for CLI and safe JSON output
are needed in jobs-worker. Default features stay disabled; use the already
resolved clap derive/std/help/usage/error-context and serde_json std feature
set. Implementation inspects Cargo resolution and updates the lockfile only
if Cargo's package dependency metadata requires it. No upgrade rationale
exists here. The wider queue comparison in
[Async Architecture](../../../docs/architecture/async.md#reopen-conditions-and-watch-list)
remains the accepted unchanged decision and is not rerun as a gate.

## Primary API checks

PostgreSQL 18 documentation confirms concurrent `nextval` produces distinct
values and aborted transactions do not reclaim them. Persist a token in the
same transaction as its job transition before reporting it. This supports
reusing the current sequence rather than adding a second version allocator.
[Sequence functions](https://www.postgresql.org/docs/18/functions-sequence.html).

Adding a column with a constant default avoids updating every existing row;
it still requires DDL lock admission and migration timeout. The history column
therefore uses constant `[]`, and the index is a separate concurrent migration.
[Modifying tables](https://www.postgresql.org/docs/18/ddl-alter.html).

Resolved source was inspected at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`:
`tokio-1.53.1/src/sync/oneshot.rs` documents receiver cancellation safety and
`Sender::is_closed`; `clap-4.6.7/src/_derive/{mod,_tutorial}.rs` documents
flatten and optional subcommands, and `clap_builder-4.6.7/src/builder/arg.rs`
documents global flags. Versioned docs.rs endpoints were unavailable through
the web tool, so resolved upstream source is the API evidence. The design
does not depend on future/latest crate behavior or on a global-flags change.

The safe supported primitives do not themselves remove a vector entry on
future cancellation: that small adapter-local Drop guard and shared admission
reference are the concrete semantic gap the design owns. This is application
ownership over existing primitives, not a new general-purpose queue library.
