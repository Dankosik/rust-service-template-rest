# Jobs reliability rollout and recovery boundary

Status: ready

This is an operational design, not authority to deploy, migrate a live
database, or invoke live recovery/discard. [Specification](spec.md) and
[system design](design/system.md) own behavior. The current request delivers
one PR; the deploying service owns the target and execution window.

## Affected graph

`migrate` applies the canonical additive history-column migration and separate
concurrent failed-index migration to PostgreSQL. Existing API/adapter producers
continue enqueue on their caller transaction. Every jobs-worker sharing that
database is a retention owner when built through Engine::new; ordinary and
publisher engines inside each corrected process share one sampler. The same
jobs-worker deliverable supplies PostgreSQL-only operator mode. NATS topology,
event schema and consumer effects do not change.

Both migrations must be new UTC-ordered versions newer than baseline
`20261002150000`; Implementation chooses the actual current UTC filenames in
order (column, then index), checks against the current base and records the
names in the owning README/profile removal manifest. Existing migrations are
never edited. The first migration adds only
`recovery_history jsonb NOT NULL DEFAULT '[]'`; the second is exactly one
`CREATE INDEX CONCURRENTLY IF NOT EXISTS background_jobs_failed_kind ON
background_jobs (kind,id) WHERE state='failed'` with the canonical
`-- no-transaction` marker. Use runner session budgets, not SQL-file SETs.

## Ordered gates

| Owner/node | Prerequisite and action | Success / distinct safe failure | Horizon and recovery | Proof/readback |
| --- | --- | --- | --- | --- |
| Delivery owner | Accepted candidate, migration metadata and affected profile source agree | Required local checks plus selected CI are actual passes / report exact missing gate | No execution gate may be replaced by a design review; no duplicate build matrix | Fixed PR commit and existing validation/CI receipts |
| Deploying database owner | Target backup/recovery authority and maintenance/capacity window; run canonical migrator | Both history versions successful and checksums match / lock timeout, deadline, invalid-index refusal or history failure stops rollout | Column has the existing 15s lock and 2m statement limits; concurrent index uses postgres.migration_deadline, default 5m, sized by target owner | `_sqlx_migrations` and valid named index readback through existing migration admission/rehearsal |
| Deploying worker owner | Schema/index admitted; replace all binaries that can run old retention for this database | Every retention owner is corrected or stopped / any old/unknown owner means custody guarantee is unavailable | Old workers remain schema-compatible but can delete seven-day failures until stopped; no migration can recover already deleted rows | Deployment inventory with image/commit plus stopped old processes; enumerate custom Engine users too |
| Deploying worker owner | Corrected fleet composed with its existing ordinary/outbox registrations and existing pool sizes | Normal admission/ready and fresh complete registered-kind sample / broker failure remains combined startup refusal; missing sample remains unavailable | Static leases and existing drain unchanged; one-slot publisher unchanged | Existing worker readiness, failed gauge and freshness, no replica sum |
| Operator | Fleet-removal gate above and PostgreSQL-only CLI admission; inspect all failed pages and unhandled pages with explicit fleet kind union | Complete traversal over unchanged data / continuation or timeout never means empty | Snapshot is current observation, repeat after concurrent change; retained unresolved rows/history grow until resolved | Payload-free pages, cursor and database observed_at |
| Operator | Reconcile possible prior ordinary/outbox effect; restore compatible handler/cause; inspect exact id/kind/version | Acknowledged redrive/discard receipt / stale, missing, conflict, failure, unknown remain distinct | Same id and publication bytes; redrive gives a fresh attempt budget; discard permanently abandons unresolved work | Inspect same identity after unknown, never infer deletion authorship from absence |

## Mixed versions and rollback

Old binaries ignore the added history column and index and continue their
existing writes; their embedded history verifier permits newer successful
migrations. New binaries require their full embedded history and refuse
missing/dirty/mismatched migrations. No worker applies or repairs schema.
Old workers can execute a redriven row without changing its archive, but **the
custody guarantee and supported recovery activation require every old
retention owner stopped**. A rolling overlap is not evidence of that gate.
Existing pending/running/failed/completed rows need no backfill.

The last rollback state preserving the new guarantee is before activating
recovery, with every old retention owner stopped. Keeping the additive schema
and rolling forward to a corrected worker remains safe. Rolling back to an old
worker can immediately resume deletion of failures already older than seven
days; it also restores early slot release and the sample-freshness race.
Therefore an emergency old-binary rollback is a known loss of the correction,
not a supported guarantee-preserving recovery. Stop worker claims/retention and
repair/roll forward if custody must remain assured. Do not down-migrate, clear
history, restart the fencing sequence or weaken embedded-history admission.

A failed concurrent build can leave an invalid index; follow the existing
migrator's exact diagnostic and authorized invalid-index cleanup procedure,
then retry that forward migration. Do not use `IF NOT EXISTS` to declare an
invalid index healthy. The archive column itself is not dropped on rollback.

Completion of this rollout requires actual target receipts for schema, corrected
fleet ownership, operational freshness and any invoked single-job action.
Those receipts are outside the current PR task. Local database/CI proof shows
the mechanisms at its tested scope, never a deployed queue or recovered event.
