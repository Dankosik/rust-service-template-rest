---
name: thermo-nuclear-code-quality-review
description: "Thermo-nuclear review: Use only on an explicit ask for a thermo-nuclear, thermonuclear, harsh, or deep code-quality audit of one fixed candidate: a diff, or a named crate or directory at one commit. Own read-only structural simplification and maintainability findings; Skip ordinary review and implementation."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Thermo-Nuclear Code Quality Review

Review one fixed candidate read-only. Be ambitious only where repository
evidence supports deleting or collapsing structure without changing accepted
behavior; do not edit the candidate.

Resolve the requested diff or area from repository context, then read its full
changed or named sources, direct callers, canonical owners, and accepted proof
boundary. Do not blend materially different candidates. Apply the shared [Review
contract](../../../docs/spec-first-workflow/shared/review.md).

An area candidate has no task packet: its verdict is `PASS`, or `CONCERNS` with
findings, each classified `FOLLOW_UP`. Every responsibility it owns counts as
affected. Nominate places to inspect from `git log --no-merges --name-only
--full-diff -- <area>`: files that repeatedly change with the area, excluding mechanical sweeps
such as a style or lint commit. Read each partner outside the area and compare
its helpers, values, and rules with the area's. A recorded decision or typed
dependency clears only the link it names, not other copies between the same
crates.

For every affected responsibility and added structure, record its anchor,
current owner, deletion or collapse attempt, observables that must survive,
next realistic change cost, available proof, and disposition. A code-judo
finding survives only with a concrete smaller replacement and an accounted
behavior boundary. Preserve error identity, status, `Option`/empty meaning,
mutation authority, cleanup and side-effect order, `Send`/`Sync` boundaries, and
dependency direction when they apply.

Treat file length, branch count, duplication, co-change, one-situation special
cases, and sequential execution as search signals, not findings. Do not prescribe extraction, a new abstraction,
parallelism, or atomicity without evidence that it reduces present ownership
or reader cost while preserving behavior.

Return findings first, ordered by outcome impact, grouping one shared cause with
every anchor its repair closes and a compact replacement. Omit cosmetic nits;
if no material finding survives, say so explicitly. Complete when every affected
responsibility and added structure has a disposition, then return [Review
Result V1](../../../docs/spec-first-workflow/interfaces/review-result-v1.md) for
the fixed candidate.
