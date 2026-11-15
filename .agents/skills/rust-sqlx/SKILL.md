---
name: rust-sqlx
description: "Durable outcome. Use when a PostgreSQL pool, transaction, commit handling, query, migration, or DB test affects durability, retries, or schema."
---

# Rust SQLx

**Durable outcome.** Follow database access from admitted connection and operation identity through execution, commit, and the caller's next safe action. Distinguish rejected work, a confirmed rollback, an uncertain commit, and cleanup of local resources.

Read `crates/infra-postgres`, `migrations/`, and the [database validation owner](../../../docs/validation/postgres.md) for the retained profile's connection, pool, repository, migration, lifecycle, and command boundaries. If a required profile is absent, report the unavailable capability to the current task owner rather than prescribing missing providers or silently adding persistence.

Keep connection sources, session configuration, credentials, and diagnostics under their admitted owner, reusing the shared pool and named operation acquisition. Keep single-statement reads on autocommit rather than wrapping every `SELECT` in `in_tx`; the migrator may own a dedicated session with a separate lifetime. Do not replace these distinctions with a universal closure wrapper or an independently created connection.

Choose a transaction for the invariant it must preserve, including isolation and locking under concurrent operations, and keep external effects and retry identity visible across the transaction boundary. Distinguish `TxError::CommitFailed`, where evidence establishes non-commit, from `TxError::CommitUnknown`, which requires idempotent reconciliation under the same operation ID; timeout, cancellation, local connection disposal, or a lost response alone cannot prove the server did not commit. Serialization failures and deadlocks reach the caller's accepted retry policy rather than a generic replay loop.

Preserve schema migration authority: applied migrations are immutable and changes are forward-only. The normal transactional migration path has a guarded nontransactional exception for statements PostgreSQL refuses inside a transaction; follow the [migration policy](../../../migrations/README.md) for replay safety, partial artifacts, and recovery. Review dynamic SQL through actual input, binding, and query contract, and keep generated metadata consistent with the accepted schema.

Select proof by the claim: configuration admission, failure classification, and rendered budget rules may have unit evidence, whereas observed transaction, locking, commit, migration, or readiness behavior requires the real-database path in the existing harness rather than a compile check or mock. Keep query compilation, schema drift, and runtime semantics as separate evidence.

This skill does not require a database run for an unrelated pure transformation or grant authority to operate production data.
