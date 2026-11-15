---
name: thermo-nuclear-code-quality-review
description: "Run a read-only structural simplification and maintainability audit of one fixed diff, crate, or directory, reporting concrete smaller replacements."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Thermo-Nuclear Code Quality Review

Review one fixed candidate read-only, proposing structural deletions or
collapses backed by repository evidence that preserve accepted behavior.

Resolve the requested diff or area from repository context, read its full
changed or named sources, direct callers, canonical owners, and accepted proof
boundary, and keep materially different candidates separate. Apply the shared
[Review contract](../../../docs/spec-first-workflow/shared/review.md).

An area candidate carries verdict `PASS` or `CONCERNS` with `FOLLOW_UP`
findings. Every responsibility it owns counts as affected. Nominate co-changing
partners from `git log --no-merges --name-only --full-diff -- <area>` (setting
aside mechanical formatting or lint sweeps), and compare each external partner's
helpers, values, and rules against the area's. A recorded decision or typed
dependency clears only the specific link it names.

For every affected responsibility and added structure, record its anchor,
current owner, deletion or collapse attempt, surviving observables, next
realistic change cost, available proof, and disposition. Keep only findings
backed by a concrete smaller replacement and an accounted behavior boundary,
preserving error identity, status, `Option`/empty meaning, mutation authority,
cleanup and side-effect order, `Send`/`Sync` bounds, and dependency direction.

Use file length, branch count, duplication, co-change, single-use branches, and
sequential execution solely as search signals; recommend an extraction,
abstraction, parallelism, or atomicity change only when evidence shows lower
present ownership or reader cost while preserving behavior.

Return findings ordered by outcome impact, grouping one shared cause with every
anchor its repair closes and a compact replacement, or state explicitly when
the candidate is clean. Finish with [Review Result
V1](../../../docs/spec-first-workflow/interfaces/review-result-v1.md) once every
affected responsibility and added structure has a disposition.
