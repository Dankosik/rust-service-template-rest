---
name: rust-sqlx
description: "Durable outcome. Use when a SQLx connection, transaction, commit result, query or migration changes what is durably written, who owns completion, or what a caller may retry."
---

# Rust SQLx

Separate the database's effect from the client's knowledge of it. A completed
query, successful closure, accepted COMMIT and observed acknowledgement are
different facts. Follow the transaction to the result the caller uses to decide
whether to respond, retry or reconcile.

## Preserve uncertainty

Under this service's transaction owner, `CommitFailed` establishes that the
transaction committed no writes; `CommitUnknown` means it may have committed.
A generic SQLx error is not interchangeable with either classified result.
Connection loss while committing is therefore not proof of rollback. Preserve
the operation's identity so uncertainty can be reconciled rather than replayed
as a fresh effect. Known rollback removes one obstacle to retry but does not
undo external effects already performed by the caller.

Retry the whole intended unit only when its effects are repeatable under the
accepted policy. Serialization failure and deadlock may justify another
transaction, but retrying one statement can violate the transaction's reads or
leave an external action duplicated. An adapter can expose the database cause;
the operation owner decides whether and how another attempt is safe.

A closure returning success can still leave an aborted transaction if it
swallowed a statement failure. Preserve the transaction owner's liveness and
commit checks instead of reconstructing success from the closure result.
Cancellation also changes knowledge: dropping a waiter does not prove that the
server stopped, and releasing local pool capacity does not prove a physical
backend disappeared.

## Assign connection and schema custody

A shared pool centralizes connection limits and lifecycle, while a dedicated
session may be necessary for session-affine behavior. Choose from the actual
session requirement and deployment allocation; opening an ad hoc connection to
avoid pool pressure defeats admission. Keep transaction completion with its
owner so repositories cannot independently commit a caller's unit of work.
Consult the resolved SQLx version's official contract when cancellation or
protocol details affect the choice.

For profile availability, DSN admission, pool/transaction ownership, exact commit
classification, query metadata and schema/migration policy, load [Persistence
Architecture](../../../docs/architecture/persistence.md). An absent profile
leaves those changes unavailable; it does not authorize rebuilding the profile
or prevent unrelated work. Retain migration history and binary/schema
compatibility under that owner rather than editing an applied migration.

For a claim about observed transaction, lock, commit, migration or readiness
behavior, load [PostgreSQL Validation](../../../docs/validation/postgres.md).
A pure classifier test can establish its mapping; only the admitted real-server
observation establishes database behavior. Compilation and offline metadata
cannot establish locking, finality or recovery.
