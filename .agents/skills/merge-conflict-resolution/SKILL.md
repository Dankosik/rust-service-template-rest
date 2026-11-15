---
name: merge-conflict-resolution
description: "Intent reconstruction. Use when a merge, rebase, cherry-pick, or revert leaves conflicted hunks needing intent recovery and resolution."
---

# Merge Conflict Resolution

**Intent reconstruction.** A conflict marker represents two intents meeting on the same lines; resolve it from the accepted outcome and canonical source of truth, never merely to clear markers.

**Forensic trace.** Before editing, inspect `git status`, the active operation (merge, rebase, cherry-pick, or revert), its base and both sides, every conflicted path, and any unrelated working-tree changes to preserve. Trace both sides of each hunk to their closest repository authority: accepted specification or task, canonical generator (`api/openapi/service.yaml` is regenerated rather than hand-merged), introducing commits and pull requests, current callers, and behavioral tests. State both intents in plain words before choosing; hold resolution on any hunk whose intents remain unclear.

**Disposition and lockfiles.** Under read-only authority, return the hunk dispositions, proof plan, and exact blocker while leaving the repository state untouched. Under change authority, combine compatible intents, and resolve genuinely conflicting hunks at hunk granularity from the accepted outcome and owning authority. Resolve `Cargo.lock` conflicts by taking the newer `Cargo.lock` side, running `cargo update --workspace`, auditing the `Cargo.lock` diff against both branches' intended pins so deliberate transitive updates survive, and validating with `--locked`. When authority leaves two conflicting intents tied, preserve the active operation and return the exact decision blocker to its owner.

**Verification and staging.** Inspect the full diff against the operation base so semantic interactions across clean textual merges surface before completion. Run the smallest behavioral proof covering the merged paths: affected-crate tests, contract drift checks when annotations or schemas moved, and `make fmt-check` when formatting shifted. Stage only the resolved paths and continue the already-authorized Git operation while leaving history rewrites and opportunistic refactors out of scope.

**Reporting.** Return the active operation and base, the traced intent sources and disposition per hunk, the executed proof, and any remaining decision blocker.
