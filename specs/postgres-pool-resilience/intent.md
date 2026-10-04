# Intent: resilient PostgreSQL pool capacity and useful diagnostics

## Problem

The requested pool assessment identified a cancellation path that can retain
pool capacity while the network silently stops answering, acquisition
diagnostics concentrated in transactions, and insufficient guidance for
budgeting connections across a deployment. The user accepted the recommended
improvements: “Окей давай сделаем то, что ты рекомендуешь.”
The user subsequently challenged application-owned pool management and asked
why a ready-made solution did not own it. This steers the accepted improvements
toward maintenance simplicity and library ownership; it does not cancel them.

## Desired outcome

Cancelled database work must not indefinitely consume pool capacity. Operators
must be able to recognize acquisition waits and timeouts beyond `in_tx`, size
the total connection budget, and assess overload and readiness recovery before
changing capacities or readiness policy.
Prefer a maintained library's connection lifecycle and the smallest lasting
application responsibility. Evaluate the total integration and maintenance cost
of a library change or a temporary dependency fix; do not build an application
pool facade merely to preserve an earlier assistant-selected mechanism.

## Affected actors and systems

Service requests, jobs and maintenance using PostgreSQL; the shared readiness
probe; operators configuring service and worker replicas; the SQLx pool and
supported direct PostgreSQL and PgBouncer transaction-mode deployments.

## Scope and non-goals

Improve the existing pool, diagnostics, operational guidance, and relevant
local proof. Library and mechanism alternatives are allowed when justified by
the accepted behavior and total maintenance cost; retaining SQLx was a technical
choice, not a user constraint. No production workload or environment was supplied;
assume the deliverable is a reusable template improvement, with local evidence
and a method for later workload-specific sizing. Production optimums, database
replacement, new deployment infrastructure, and general database tuning are
outside scope.

## Constraints

Preserve PostgreSQL 14+, supported PgBouncer deployments, commit-unknown and
transaction safety, and optional profiles. Preserve current pool capacities
and readiness policy unless discriminating evidence warrants a separately
recorded decision. Local edits, builds, real-database diagnostics and local
integration are authorized; remote writes, spending and deployment are not.
Work in the clean task worktree; preserve unrelated main-checkout edits.

## Success signal

A cancelled active operation no longer leaves a permanently occupied pool
slot; acquisition pressure outside transactions is diagnosable; the documented
budget accounts for all connection owners; bounded local overload and recovery
evidence supports the retained policy without claiming production capacity.
