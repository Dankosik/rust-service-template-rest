# PostgreSQL maintenance observation

Use this procedure for an explicitly authorized, read-only observation of a
chosen database, role, schema and time window. It adds no background collector
or permission to access a deployment. PostgreSQL 14 is the baseline; select the
checkpoint block for the observed server major, not the local compose image.

## Cleanup signals

The existing pass operation reports these instruments through the process's
metrics recorder, including direct and concurrent calls. Each has a finite
`cleanup` label; retained family sections below name its values.

| Instrument | Additional labels | Unit and meaning |
| --- | --- | --- |
| `postgres_cleanup_active_passes` | none | Gauge of started, unterminated passes in this process. |
| `postgres_cleanup_committed_batches_total` | none | Counter of confirmed committed batches, including an empty final batch. |
| `postgres_cleanup_removed_rows_total` | none | Counter of confirmed committed deleted rows. |
| `postgres_cleanup_passes_total` | `outcome` | Counter of terminated passes: `completed`, `failed`, `cancelled`. |
| `postgres_cleanup_pass_duration_seconds` | `outcome` | Histogram of monotonic elapsed seconds from polled pass entry through termination, including admission and database waits. |

Batch and row counters advance during a pass. `completed` means the loop
returned after a batch shorter than 500; `failed` means it returned an error;
`cancelled` means an active pass future was dropped. Waiting for the next tick,
or creating an unpolled future, starts no pass. Concurrent calls each contribute
one to the active gauge until their own termination. A missing series before
first use, failed scrape or crashed process is not observed idle or success.

One info-level `postgres_cleanup_pass_finished` event reports `cleanup`,
`outcome`, `elapsed_seconds`, `committed_batches` and `removed_rows` for each
terminated pass, without SQL, payloads or raw errors. Earlier confirmed batches
remain counted when a later batch fails or is cancelled. An unknown commit may
have durable effects absent from these known-committed totals: they cannot
reconcile inventory. Abrupt process or collector loss can lose final evidence.

Counters reset on process restart. Use reset-aware rates and scrape health when
aggregating replicas; active gauges sum current active passes only, not backlog,
database transactions or capacity. Existing removed-row counters overlap the new
row counter: do not add aliases of the same deletion. The duration histogram's
highest finite default bucket is 10 seconds; long-pass quantiles have limited
resolution. The event and histogram sum retain elapsed seconds.

## Choose and bound an observation

Record target identity, intended role/schema, window start/end, server major,
visibility restrictions and each command's exit status alongside its output.
Select an existing libpq service entry with the authorized host, database, role
and TLS policy, and an existing protected password file or credential mechanism.
Do not put credentials in command arguments or print connection strings.
Put exactly one selected SQL block below in a local `observation.sql` file, then
run it once (replace the example service and schema with the chosen values):

```sh
psql -X --no-password --set=ON_ERROR_STOP=1 --set=target_schema=public \
  --dbname='service=maintenance_readonly application_name=maintenance_observer connect_timeout=5' \
  --file=observation.sql
```

Each block is one short read-only transaction with a 2-second statement budget,
250-ms lock budget and 5-second idle-in-transaction budget. These are observer
session examples, not application keys or server tuning. `psql -X` excludes
startup-file commands. A connection or query error exits nonzero and ends the
session; record **unavailable**, never zero, and retain the error class without
credentials. Disconnect (or explicitly roll back before disconnecting if using
an interactive session) after failure. Do not retry automatically. End the
session between manually chosen observations; never sleep in a transaction.
The client/statement budget semantics are documented in
[PostgreSQL 14 client defaults](https://www.postgresql.org/docs/14/runtime-config-client.html).

Each SELECT returns its database, server version and `observed_at`. Catalogue
samples also include database `stats_reset`; WAL, checkpoint and archiver samples
carry their own cluster reset. Repeat manually at the end of the chosen window.
Compare counters only for the same target and unchanged reset boundaries;
negative/reset deltas are unavailable. Relation counters can be reset separately
without a per-table reset timestamp in these views: require a known no-reset
window, not merely unchanged database `stats_reset`. Cumulative statistics can
lag activity; an old observation keeps its original timestamp.

Permissions, row security, provider filtering, disabled statistics and null
fields can make evidence incomplete. A successful empty visible set does not
prove global absence when visibility is incomplete. Confirm visibility with the
selected operator role; this procedure grants no roles or privileges.

## Expired backlog

Run only the retained family's block against its actual schema. These ordinary
MVCC reads neither lock eligible rows nor skip locked rows. Each returns one
row even for a successful empty set (`oldest_eligible_at` is then null).
`eligible_rows_capped = 1000` and `cap_reached = true` mean **at least 1000**.
Sorting eligible timestamps before limiting preserves the oldest timestamp in
that statement snapshot. A 1000-row cap bounds the aggregate input, not physical
I/O: dead tuples, visibility checks, traversal and sorting can still cost work.
A timeout is unavailable evidence. Separate snapshots and concurrent deletion
do not reconcile an inventory; a successful short SKIP LOCKED cleanup batch may
leave eligible locked rows visible here.

<!-- template:begin jobs:docs-maintenance-jobs -->
### Completed jobs

`cleanup="jobs"` covers completed-job retention. The worker keeps successes
for 24 hours and repeats 500-row committed batches. Failed and unhandled jobs
are a separate custody workload; use the existing payload-free
[inspection and recovery procedure](background-jobs.md#inspect-and-recover-retained-jobs).
Explicit redrive/discard owns their disposition. Current heap autovacuum storage
parameters are threshold 5000 and scale factor 0; inspect TOAST separately.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH eligible AS MATERIALIZED (
    SELECT finished_at
    FROM :"target_schema".background_jobs
    WHERE state = 'completed'
      AND finished_at <= statement_timestamp() - interval '24 hours'
    ORDER BY finished_at
    LIMIT 1000
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       min(finished_at) AS oldest_eligible_at,
       count(*) AS eligible_rows_capped,
       1000 AS cap, count(*) = 1000 AS cap_reached
FROM eligible;
COMMIT;
```

<!-- template:end jobs:docs-maintenance-jobs -->

<!-- template:begin http-idempotency:docs-maintenance-http-idempotency -->
### HTTP idempotency

`cleanup="http_idempotency"` covers expired replay records. Their stored expiry
reflects the configured retention; see [HTTP idempotency](http-idempotency.md).
Current heap and TOAST vacuum settings are threshold 5000 and scale factor 0.01.
The threshold-plus-scale formula is not always earlier than server defaults:
compare effective settings, table estimates and the server-major cap.
`http_idempotency_cleanup_runs_total` remains scheduler-only completed/failed;
`http_idempotency_cleanup_removed_records_total` remains committed-batch rows.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH eligible AS MATERIALIZED (
    SELECT expires_at
    FROM :"target_schema".http_idempotency_records
    WHERE expires_at <= statement_timestamp()
    ORDER BY expires_at
    LIMIT 1000
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       min(expires_at) AS oldest_eligible_at,
       count(*) AS eligible_rows_capped,
       1000 AS cap, count(*) = 1000 AS cap_reached
FROM eligible;
COMMIT;
```

<!-- template:end http-idempotency:docs-maintenance-http-idempotency -->

<!-- template:begin inbound-webhooks:docs-maintenance-inbound-webhooks -->
### Webhook receipts

`cleanup="webhook_receipts"` covers receipts strictly older than 1,209,600 elapsed
seconds (14 × 24 hours), matching the runtime duration. A calendar-day interval
can differ across daylight-saving transitions in the observer's timezone.
See [inbound webhooks](inbound-webhooks.md). Receipt vacuum policy inherits
server settings unless an operator supplied table/TOAST overrides.
`webhook_receipt_cleanup_runs_total` remains scheduler-only completed/failed;
`webhook_receipt_cleanup_removed_receipts_total` remains committed-batch rows.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH eligible AS MATERIALIZED (
    SELECT received_at
    FROM :"target_schema".webhook_receipts
    WHERE received_at < statement_timestamp() - 1209600::bigint * interval '1 second'
    ORDER BY received_at
    LIMIT 1000
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       min(received_at) AS oldest_eligible_at,
       count(*) AS eligible_rows_capped,
       1000 AS cap, count(*) = 1000 AS cap_reached
FROM eligible;
COMMIT;
```

<!-- template:end inbound-webhooks:docs-maintenance-inbound-webhooks -->

For each retained owner, the first scheduler tick is immediate; subsequent ticks
use a 60-second interval with delayed missed-tick behavior. A pass repeats full
500-row batches without pacing or a pass-wide budget. Each batch retains its
existing transaction and statement limits. Deletion creates reusable-space work
for vacuum; it does not promise a smaller file. WAL generation and retention
are separate from row deletion and vacuum reuse.

## Catalogue observations

These catalogue-only queries tolerate absent optional tables. They select only
`background_jobs`, `http_idempotency_records` and `webhook_receipts` in the
selected schema, then follow catalogue OIDs to their TOAST and indexes. Listing
an absent name does not execute a backlog query against it. Empty `observations`
is a successful visible set, subject to the visibility limitations above.
Relation sizes and statistics are database-local; they are not exact bloat or
physical I/O. The size functions' accounting is defined in
[PostgreSQL administrative functions](https://www.postgresql.org/docs/14/functions-admin.html).

### Heap, TOAST, index size and churn

Parent `table_bytes_including_toast` already includes TOAST and its index;
`total_bytes_including_indexes` includes all attached indexes too. The separate
TOAST and index rows provide attribution: do not sum them into the parent total
again. Live/dead row values are estimates; block reads/hits describe PostgreSQL's
cache activity, not device reads. Counters need a reset-safe observation window.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH roots AS (
    SELECT c.oid, c.reltoastrelid
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = :'target_schema' AND c.relkind IN ('r', 'p')
      AND c.relname IN ('background_jobs', 'http_idempotency_records', 'webhook_receipts')
), heaps AS (
    SELECT oid, 'heap'::text AS relation_role FROM roots
    UNION ALL
    SELECT reltoastrelid, 'toast' FROM roots WHERE reltoastrelid <> 0
), sample AS (
    SELECT c.oid, n.nspname AS schema_name, c.relname, h.relation_role,
           pg_relation_size(c.oid, 'main') AS main_bytes,
           pg_table_size(c.oid) AS table_bytes_including_toast,
           pg_indexes_size(c.oid) AS attached_index_bytes,
           pg_total_relation_size(c.oid) AS total_bytes_including_indexes,
           s.n_live_tup, s.n_dead_tup, s.n_tup_ins, s.n_tup_upd,
           s.n_tup_hot_upd, s.n_tup_del, s.seq_scan, s.idx_scan,
           io.heap_blks_read, io.heap_blks_hit,
           io.idx_blks_read, io.idx_blks_hit,
           io.toast_blks_read, io.toast_blks_hit,
           io.tidx_blks_read, io.tidx_blks_hit
    FROM heaps h JOIN pg_class c ON c.oid = h.oid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_all_tables s ON s.relid = c.oid
    LEFT JOIN pg_statio_all_tables io ON io.relid = c.oid
    ORDER BY c.oid
    LIMIT 6
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

Indexes belonging to those heap/TOAST OIDs:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH roots AS (
    SELECT c.oid, c.reltoastrelid
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = :'target_schema' AND c.relkind IN ('r', 'p')
      AND c.relname IN ('background_jobs', 'http_idempotency_records', 'webhook_receipts')
), heaps AS (
    SELECT oid, 'heap'::text AS relation_role FROM roots
    UNION ALL
    SELECT reltoastrelid, 'toast' FROM roots WHERE reltoastrelid <> 0
), sample AS (
    SELECT i.indrelid AS table_oid, i.indexrelid AS index_oid,
           n.nspname AS index_schema, c.relname AS index_name,
           pg_relation_size(c.oid) AS index_main_bytes,
           i.indisvalid, i.indisready,
           s.idx_scan, s.idx_tup_read, s.idx_tup_fetch
    FROM heaps h JOIN pg_index i ON i.indrelid = h.oid
    JOIN pg_class c ON c.oid = i.indexrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_all_indexes s ON s.indexrelid = i.indexrelid
    ORDER BY i.indrelid, i.indexrelid
    LIMIT 50
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

At 50 index rows the list may be truncated; record that limitation.

### Vacuum, analyze, freeze and effective settings

Inspect parent and TOAST `reloptions` independently. PostgreSQL uses the TOAST
option set when present; only an absent TOAST option set inherits the main
table's option set. Within the selected set, an unspecified vacuum threshold,
scale factor or maximum uses the server default. Thus an unrelated TOAST override
can stop inheritance of a main-table vacuum threshold. The query selects the
option set before resolving those three values and shows raw options alongside
them; the maximum is null when unsupported, and -1 means no maximum. Other
options must be interpreted with their own documented fallback and bounds,
including freeze caps and cost-policy inheritance. See
[table storage parameters](https://www.postgresql.org/docs/18/sql-createtable.html#SQL-CREATETABLE-STORAGE-PARAMETERS).
For whole-set TOAST inheritance, the execution authority is `table_recheck_autovac`
in [PostgreSQL 14](https://github.com/postgres/postgres/blob/REL_14_STABLE/src/backend/postmaster/autovacuum.c#L2868-L2882)
and [PostgreSQL 18](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/postmaster/autovacuum.c#L2768-L2784):
parent options are selected only when `extract_autovac_opts` returns null.
These maintained-major source references distinguish actual selection from the
general CREATE TABLE wording; they are not immutable revision receipts.
A null last-vacuum/analyze timestamp or disabled tracking does not prove health.
Transaction-ID and multixact ages expose horizons, not time durations.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH roots AS (
    SELECT c.oid, c.reltoastrelid
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = :'target_schema' AND c.relkind IN ('r', 'p')
      AND c.relname IN ('background_jobs', 'http_idempotency_records', 'webhook_receipts')
), heaps AS (
    SELECT oid, 'heap'::text AS relation_role FROM roots
    UNION ALL
    SELECT reltoastrelid, 'toast' FROM roots WHERE reltoastrelid <> 0
), sample AS (
    SELECT c.oid, n.nspname AS schema_name, c.relname, h.relation_role,
           c.reloptions, parent.reloptions AS main_table_options,
           COALESCE(
               (SELECT option_value FROM pg_options_to_table(COALESCE(c.reloptions, parent.reloptions))
                WHERE option_name = 'autovacuum_vacuum_threshold'),
               current_setting('autovacuum_vacuum_threshold', true)
           ) AS effective_vacuum_threshold,
           COALESCE(
               (SELECT option_value FROM pg_options_to_table(COALESCE(c.reloptions, parent.reloptions))
                WHERE option_name = 'autovacuum_vacuum_scale_factor'),
               current_setting('autovacuum_vacuum_scale_factor', true)
           ) AS effective_vacuum_scale_factor,
           COALESCE(
               (SELECT option_value FROM pg_options_to_table(COALESCE(c.reloptions, parent.reloptions))
                WHERE option_name = 'autovacuum_vacuum_max_threshold'),
               current_setting('autovacuum_vacuum_max_threshold', true)
           ) AS effective_vacuum_max_threshold,
           age(c.relfrozenxid) AS frozen_xid_age,
           mxid_age(c.relminmxid) AS min_multixact_age,
           s.last_vacuum, s.last_autovacuum, s.last_analyze, s.last_autoanalyze,
           s.vacuum_count, s.autovacuum_count, s.analyze_count,
           s.autoanalyze_count, s.n_mod_since_analyze,
           v.pid AS vacuum_pid, v.phase AS vacuum_phase,
           v.heap_blks_total, v.heap_blks_scanned, v.heap_blks_vacuumed
    FROM heaps h JOIN pg_class c ON c.oid = h.oid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_class parent ON parent.reltoastrelid = c.oid
         AND parent.oid IN (SELECT oid FROM roots)
    LEFT JOIN pg_stat_all_tables s ON s.relid = c.oid
    LEFT JOIN pg_stat_progress_vacuum v ON v.relid = c.oid
         AND v.datid = (SELECT oid FROM pg_database WHERE datname = current_database())
    ORDER BY c.oid
    LIMIT 6
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT d.datname, age(d.datfrozenxid) AS database_frozen_xid_age,
           mxid_age(d.datminmxid) AS database_min_multixact_age
    FROM pg_database d WHERE d.datname = current_database()
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

This named lookup keeps absent version-specific settings visible as null,
including PostgreSQL 18's vacuum maximum threshold. It reads settings only.
The baseline progress fields above are common across supported majors; newer
progress counters and PostgreSQL 18 `pg_stat_io` WAL-object fields are deliberately
not prerequisites. See [vacuum progress](https://www.postgresql.org/docs/14/progress-reporting.html#VACUUM-PROGRESS-REPORTING)
and [PostgreSQL 18 vacuum settings](https://www.postgresql.org/docs/18/runtime-config-vacuum.html).

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT wanted.name, s.setting, s.unit, s.source, s.pending_restart
    FROM (VALUES
        ('autovacuum'), ('track_counts'), ('track_activities'),
        ('track_io_timing'), ('track_wal_io_timing'),
        ('autovacuum_max_workers'), ('autovacuum_naptime'),
        ('autovacuum_vacuum_threshold'), ('autovacuum_vacuum_scale_factor'),
        ('autovacuum_vacuum_max_threshold'),
        ('autovacuum_vacuum_insert_threshold'), ('autovacuum_vacuum_insert_scale_factor'),
        ('autovacuum_analyze_threshold'), ('autovacuum_analyze_scale_factor'),
        ('autovacuum_vacuum_cost_delay'), ('autovacuum_vacuum_cost_limit'),
        ('vacuum_cost_delay'), ('vacuum_cost_limit'),
        ('autovacuum_freeze_max_age'), ('autovacuum_multixact_freeze_max_age'),
        ('vacuum_freeze_min_age'), ('vacuum_freeze_table_age'),
        ('vacuum_multixact_freeze_min_age'), ('vacuum_multixact_freeze_table_age'),
        ('max_wal_size'), ('min_wal_size'), ('checkpoint_timeout'),
        ('checkpoint_completion_target'), ('wal_keep_size'),
        ('max_slot_wal_keep_size'), ('archive_mode')
    ) AS wanted(name)
    LEFT JOIN pg_settings s ON s.name = wanted.name
    ORDER BY wanted.name
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

Table/TOAST overrides can change thresholds, scale factors, freeze ages and
cost policy. The effective delete/update trigger uses threshold and scale factor
with the table estimate, and on PostgreSQL 18 also the applicable maximum
threshold. Trigger eligibility is not a guarantee of timely completion: worker
availability, resource pressure and old horizons matter. Do not infer that
5000 + 1% always runs earlier than defaults or that deleting rows returns disk.

### Transaction horizons and blockers

The activity query is database-filtered, excludes this observer and considers
at most 50 sessions before asking for blockers. It returns no query text.
`blocking_pids` can name sessions outside the selected database; that is a lead
for separately scoped inspection, not authority to inspect or terminate them.
Hidden fields remain incomplete evidence. The prepared-transaction and slot
queries that follow are **cluster-scoped**, with database where available;
use them only when the observation's authorized scope includes those metadata.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT a.pid, a.backend_type, a.state,
           statement_timestamp() - a.xact_start AS transaction_age,
           age(a.backend_xid) AS backend_xid_age,
           age(a.backend_xmin) AS backend_xmin_age,
           a.wait_event_type, a.wait_event, pg_blocking_pids(a.pid) AS blocking_pids
    FROM (
        SELECT pid, backend_type, state, xact_start, backend_xid, backend_xmin,
               wait_event_type, wait_event
        FROM pg_stat_activity
        WHERE datname = current_database() AND pid <> pg_backend_pid()
        ORDER BY xact_start NULLS LAST, pid
        LIMIT 50
    ) a
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT database AS prepared_database, transaction AS prepared_xid,
           age(transaction) AS prepared_xid_age, prepared,
           statement_timestamp() - prepared AS prepared_age
    FROM pg_prepared_xacts
    ORDER BY prepared, transaction
    LIMIT 50
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT slot_type, database AS slot_database, active, active_pid,
           xmin, age(xmin) AS xmin_age, catalog_xmin,
           age(catalog_xmin) AS catalog_xmin_age, restart_lsn,
           confirmed_flush_lsn, wal_status, safe_wal_size,
           pg_is_in_recovery() AS in_recovery,
           CASE WHEN NOT pg_is_in_recovery()
                THEN pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn)
           END AS primary_restart_distance_bytes
    FROM pg_replication_slots
    ORDER BY restart_lsn NULLS LAST, slot_type, database
    LIMIT 50
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

At 50 rows any of these lists can be truncated. Prepared transactions retain
horizons independently of active sessions; no GIDs are emitted. Slot
`restart_lsn` distance estimates WAL that may need retention on the primary,
not attributable query WAL or an exact on-disk total. The CASE invokes the
primary-only current-WAL function only outside recovery; a null distance on a
standby is unavailable, not zero. See the
[PostgreSQL 14 slot view](https://www.postgresql.org/docs/14/view-pg-replication-slots.html).

### WAL and checkpoints

These counters are cluster-scoped, even though the output identifies the
observer's connected database. They cannot assign WAL to the selected tables.
Use elapsed window seconds for rates and each view's own reset timestamp.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT wal_records, wal_fpi, wal_bytes, wal_buffers_full, stats_reset
    FROM pg_stat_wal
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

PostgreSQL **14–16 only** checkpoint block:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT checkpoints_timed, checkpoints_req,
           checkpoint_write_time AS write_time_ms,
           checkpoint_sync_time AS sync_time_ms,
           buffers_checkpoint AS buffers_written, stats_reset
    FROM pg_stat_bgwriter
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

PostgreSQL **17 and later only** checkpoint block. Run this instead of the
14–16 block; a CASE cannot protect references to columns absent on that server.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT num_timed, num_requested, write_time AS write_time_ms,
           sync_time AS sync_time_ms, buffers_written, stats_reset
    FROM pg_stat_checkpointer
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

The split follows [PostgreSQL 17 checkpointer statistics](https://www.postgresql.org/docs/17/monitoring-stats.html#MONITORING-PG-STAT-CHECKPOINTER-VIEW).
Time counters are milliseconds; buffer counters count blocks. Neither measures
application latency. Requested checkpoint growth is a correlation to investigate
with WAL volume, disk and provider telemetry, not a tuning instruction.

### Replication and archive retention

These are cluster/provider observations, not per-table measurements. The sender
query caps its visible set at 50. A null lag or empty sender/receiver set does
not establish zero lag or healthy replication; topology, privileges and provider
management matter. The receiver block omits connection information and hostnames.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT pid, state, sent_lsn, write_lsn, flush_lsn, replay_lsn,
           write_lag, flush_lag, replay_lag, sync_state, reply_time
    FROM pg_stat_replication
    ORDER BY pid
    LIMIT 50
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT pid, status, receive_start_lsn, receive_start_tli,
           written_lsn, flushed_lsn, received_tli,
           last_msg_send_time, last_msg_receipt_time, latest_end_lsn, latest_end_time
    FROM pg_stat_wal_receiver
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2s';
SET LOCAL lock_timeout = '250ms';
SET LOCAL idle_in_transaction_session_timeout = '5s';
WITH sample AS (
    SELECT archived_count, last_archived_time, failed_count,
           last_failed_time, stats_reset
    FROM pg_stat_archiver
)
SELECT current_database() AS database_name,
       current_setting('server_version') AS server_version,
       statement_timestamp() AS observed_at,
       (SELECT stats_reset FROM pg_stat_database
        WHERE datname = current_database()) AS database_stats_reset,
       COALESCE(jsonb_agg(to_jsonb(sample)), '[]'::jsonb) AS observations
FROM sample;
COMMIT;
```

Archive success/failure times and deltas must be interpreted with effective
`archive_mode` and provider policy. Empty/null observations are not proof that
archiving is disabled. A slot or archive backlog can retain WAL after row
cleanup has completed. For baseline field meanings, visibility and statistics
freshness see [PostgreSQL 14 monitoring](https://www.postgresql.org/docs/14/monitoring-stats.html).

## Interpret the same window

Correlate both snapshots with existing HTTP/job latency and error rates, pool
waits, queue age, and host/provider CPU, I/O and disk observations from the same
window. Preserve unavailable, stale and incomplete states in the report.
Known committed cleanup progress, a remaining eligible backlog and growing WAL
can coexist without contradiction. None alone proves capacity or database health.

Use [fleet-wide connection allocation](architecture/persistence.md#connection-allocation)
for replicas, rolling overlap, worker pools, dedicated listeners, poolers and
operator reserve. A default pool ceiling is not a fleet-capacity result.

Native estimates do not establish exact bloat; a separately authorized
`pgstattuple` inspection can address that gap at a relation-scan cost. Native
cluster WAL counters cannot attribute query-level WAL; separately authorized
`pg_stat_statements` instrumentation may address attribution, with configuration,
collection and reset-history costs. This procedure installs neither extension,
resets no statistics, runs no EXPLAIN ANALYZE or VACUUM/ANALYZE, terminates no
sessions and changes no server/table policy. `pg_repack` is a deferred corrective
operation, not a read-only diagnostic. Reopen operational scope before such work.
