# Storage transition and recovery

Status: ready. This is the deployable contract to document; no live rollout is
authorized. The affected graph is producer adapters → PostgreSQL background_jobs
← jobs-worker. The migrated binaries, embedded migrator and selected jobs profile
must agree. Old bytea binaries cannot overlap new JSONB binaries on this table.

| Owner/node | Prerequisite and action | Success / safe failure | Horizon and recovery | Proof/readback |
| --- | --- | --- | --- | --- |
| Operator / all producers and workers | Inventory every producer and worker using this DB; choose a maintenance window and build the new migration/code together. Run read-only preflight through an approved DB session. | UTF8 and every payload converts; every key converts without byte substitution. A conversion error is a refusal, never permission to repair data. | Reads have an operator-selected finite statement timeout; timeout leaves preflight incomplete. | `SHOW server_encoding`; force evaluation of `convert_from(payload,'UTF8')::jsonb` and nullable keys across all rows, returning only counts/IDs, never payloads or key contents. `count(*)` alone is insufficient because the optimizer can remove unused conversion expressions. |
| Operator / producer and worker graph | Stop producer ingress/background producers, disable worker readiness and stop all workers; await their bounded shutdown. | All binaries that can access this table are stopped. Failure to quiesce aborts the migration attempt. | Use existing per-process grace; surviving running rows may remain and are preserved. | Deployment inventory and stopped process status for every producer/worker, not one service. |
| Existing embedded migrator / PostgreSQL | Apply the new forward file in its existing one-transaction, lock/statement/total deadlines. The file takes the table lock, rechecks UTF8, converts columns, adds tracestate and replaces the running index. | Success records the new migration/checksum and schema together. Invalid bytes/JSON/NUL/numeric range/encoding, lock timeout or statement failure rolls back the entire file and its history entry. | Last rollback-safe state is before successful migration commit. A refused run can resume the old binaries only after confirming old schema/history; no row repair is implicit. | Read migration history and column/index types after the runner's outcome. For an unknown runner outcome, inspect schema/history before choosing a binary. |
| Operator / new producers and workers | Only after confirmed migration, start new binaries; worker startup verifies UTF8 and schema. | Readiness and normal enqueue/claim/completion with the new representation. Failure keeps affected process unavailable and chooses compatible forward repair. | After successful conversion, old binaries/down-migration are not recovery. No automatic downgrade or data deletion. | Per-node deployed identity, admission and durable queue path; live proof belongs to a separately authorized rollout. |

The forward file changes no job identity, timing, attempt count, generation,
state or terminal history. JSONB intentionally normalizes formatting, duplicate
keys and numeric rendering. A legacy payload containing an unrepresentable value
is not silently sanitized. Existing unique keys convert under explicit C
collation and retain exact equality; old nullable traceparent remains untouched.
Preflight is advisory against current data; the migration repeats authoritative
conversion after quiescence. If a bounded window cannot convert the population,
stop and reopen this rollout mechanism with measured size/time evidence, rather
than introducing an online dual-format backfill during Implementation.
