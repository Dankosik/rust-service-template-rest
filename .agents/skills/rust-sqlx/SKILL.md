---
name: rust-sqlx
description: "Durable outcome. Use when a PostgreSQL pool, transaction, commit handling, query, migration, or DB test affects durability, retries, or schema."
---

# Rust SQLx

**Durable outcome.** Trace database work from connection admission and operation identity through execution, commit, and caller recovery, separating rejected inputs, confirmed rollbacks, uncertain commits, and local resource cleanup.

**Profile owners.** Follow `crates/infra-postgres`, `crates/migrate`, `migrations/`, and `justfile` for pool, acquisition, transaction, migration, and integration-test boundaries; if the active service profile omits PostgreSQL, report that capability boundary to the task owner.

**Pool and session scope.** Acquire connections through `infra_postgres::acquire` or `infra_postgres::in_tx` on the shared `infra_postgres::Pool` (which hides raw `sqlx::PgPool::begin`, `acquire`, and `Executor`), keeping single-statement reads on autocommit and reserving dedicated sessions for the migrator.

**Transactions and commit uncertainty.** Scope transactions to the concurrency invariant and locks they protect, keeping external side effects and idempotency keys visible across the boundary. Separate `TxError::CommitFailed` (confirmed rollback) from `TxError::CommitUnknown` (timeout, cancellation, or dropped connection during commit, requiring idempotent reconciliation under the same operation ID), and route serialization or deadlock errors to the caller's retry policy.

**Migrations and queries.** Keep applied migrations immutable and forward-only (`YYYYMMDDHHMMSS_<snake_case>.sql`), enforced by `cargo test -p migrate` and `just migration-check` (`scripts/checks/migrations.py`), using `-- no-transaction` with one idempotent statement only where PostgreSQL forbids transactions (`CREATE INDEX CONCURRENTLY`) and leaving session timeouts to `crates/migrate`. Bind dynamic SQL parameters safely and update `.sqlx/` via `just sqlx-prepare` when `sqlx::query!` statements change.

**Proving layer.** Prove pure config, error classification, and budget math with unit tests, and run `just test-integration-db` whenever transaction, locking, commit, migration, or readiness behavior is part of the claim, keeping production data strictly out of local test scope.
