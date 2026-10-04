# Intent: close the infra-postgres reliability gaps

Status: ready

## Problem

The architecture review found a source-backed connection recovery gap,
uncomposed default request/database budgets, a borrowed-connection test that
does not exercise its claimed path, and obsolete persistence documentation.
Leaving these half-correct would require another repair of the same behavior.

## Desired outcome

Fully repair the necessary findings and open one separate pull request containing
the fixes, matching regression evidence, and accurate documentation. The user
authorized scoped implementation, local validation, commit, push, and that PR.

## Affected actors and systems

HTTP callers, transaction callers (including HTTP idempotency and background
jobs), operators, and maintainers of the optional PostgreSQL profile.

## Scope and non-goals

Scope is the infra-postgres review findings and consumers actually affected by
their repair. Retain SQLx and established transaction truth, deployment support,
and configuration ownership. No driver replacement, speculative performance
rewrite, universal readiness/TLS redesign, schema redesign, merge, deployment,
paid experiment, or unrelated cleanup is requested.

## Constraints

Use the isolated `codex/infra-postgres-closeout-20261002` worktree based on
`67be869acea112af271ec8ba621cbc50ae9d36b7`; preserve other work. Technical choices
and their proof are agent-owned. A source-backed risk is not a production
incident or an observed blackhole experiment. Apply the repository's validation
budget and existing release gates, with explicit limits on every evidence claim.

## Success signal

The necessary defects are closed in one reviewable PR, transaction safety and
supported consumer behavior remain intact, relevant required checks have actual
results, and remaining external observations are accurately distinguished from
the completed local work. No unresolved requester-owned decision remains.
