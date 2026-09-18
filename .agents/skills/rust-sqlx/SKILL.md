---
name: rust-sqlx
description: "Durable outcome. Use when a Rust service PostgreSQL pool, DSN, transaction, commit result, repository query, migration, or database-backed test can change what is durably written, what a caller may retry, or which schema a binary carries."
---

# Rust SQLx

**Durable outcome.** Every database decision is judged by what is durably written and what the caller may safely do next; a query that compiles and a test that passes without a server prove neither. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Decide against the existing owners. infra_postgres::Dsn admits the one connection string and refuses what sqlx would otherwise merge: libpq environment, passfile, sockets, allow and prefer, TLS files, unknown parameters; a diagnostic names the rule, never the value. Budgets are constants in that crate (acquire, statement and idle-in-transaction session defaults, rollback, migration lock and statement, run deadline); postgres.max_connections is the only operator value. A new budget or key needs a deployment reason the persistence document records, not a convenience.

The pool is opened by bootstrap when the profile is enabled, joins readiness through PostgresProbe, and closes in the dependency-close stage; an adapter never opens its own connection. Every access runs through in_tx or in_tx_with with the async closure receiving the connection, so the closure cannot commit or roll back on its own. Read the commit result exactly: CommitFailed means nothing was written and a retry is allowed; CommitUnknown means the server may have committed and the caller reconciles against the operation's own identity. retryable names serialization failures and deadlocks; there is no retry loop, because safety depends on what the caller already did. SqlSafeStr makes a dynamic SQL string a compile error; AssertSqlSafe is a review item with a reason beside it.

Schema evolves first. A migration is a new forward-only file, one transaction, snake_case name, never an edit of an applied one; sqlx compares checksums against the live history and make migration-check refuses the rewrite before it reaches a database. The migrate crate embeds the set and its build script re-runs on a new file; the image runs the same binary as /migrate, so a schema change is rehearsed with make migration-validate, not assumed from a passing unit test. Access code adapts to the schema; a query never drives a migration.

Proof needs a server for the claim it makes. Admission, budget rendering, commit classification, source rules, and stage mapping are unit tests without Docker. Observed transaction, lock, commit, readiness, or migration behavior lives in test/tests behind the integration feature, each test in its own #[sqlx::test] database, run through ALLOW_HEAVY=1 make test-integration-db; a test that would pass without DATABASE_URL is not that proof.

For review, trace each write to its transaction boundary and its commit-result handling, each connection to the admitted DSN and the shared pool, and each schema change to a new migration and its rehearsal, without editing. For implementation, add at the owner, keep unit tests Docker-free, run the database proof when the claim is behavioral, and report which server observed it.
