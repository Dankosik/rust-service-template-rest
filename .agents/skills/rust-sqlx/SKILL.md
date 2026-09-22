---
name: rust-sqlx
description: "Durable outcome. Use when a Rust service PostgreSQL pool, DSN, transaction, commit result, repository query, migration, or database-backed test can change what is durably written, what a caller may retry, or which schema a binary carries."
---

# Rust SQLx

**Durable outcome.** Every database decision is judged by what is durably written and what the caller may safely do next; a query that compiles and a test that passes without a server prove neither. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

First load the service-local persistence architecture and database validation owner. When the selected service includes PostgreSQL, they name its connection, repository, migration, lifecycle, and command owners. That connection admission refuses ambient or conflicting sources, and its diagnostic names the rule, never the value. A new budget or key needs a deployment reason the local persistence record names, not a convenience.

When the profile is available, its pool joins readiness and closes before the runtime is torn down; an adapter never opens its own connection. Every access stays within the repository's transaction boundary, so a closure cannot commit or roll back on its own. Read the commit result exactly: a failed commit means no effect is established, while an unknown result may have committed and requires reconciliation against the operation's own identity. Serialization failures and deadlocks need explicit caller policy; there is no generic retry loop because safety depends on what the caller already did. Dynamic SQL needs an explicit review reason.

When migrations are part of the selected profile, schema evolves first. A migration is a new forward-only file, one transaction, never an edit of an applied one; the service-local migration owner refuses a rewrite before it reaches a database. Access code adapts to the schema; a query never drives a migration. If the profile is absent, stop at that availability record and do not prescribe missing providers, migrations, or commands.

Proof needs a server for the claim it makes. Admission, budget rendering, commit classification, source rules, and stage mapping may be unit tests without Docker. Observed transaction, lock, commit, readiness, or migration behavior uses the service's real PostgreSQL validation path; a test that would pass without that server is not that proof.

For review, trace each write to its transaction boundary and its commit-result handling, each connection to the local admitted source and shared pool, and each schema change to a new migration and its rehearsal, without editing. For implementation, add at the owner, keep unit tests Docker-free, run database proof when the claim is behavioral, and report which server observed it.
