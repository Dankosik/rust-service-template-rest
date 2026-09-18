---
name: merge-conflict-resolution
description: "Intent reconstruction. Use when an in-progress merge, rebase, cherry-pick, or revert has conflicted hunks that need both sides' intent recovered, a resolution the accepted outcome supports, proof, and continuation of the operation."
---

# Merge Conflict Resolution

**Intent reconstruction.** A conflict marker is two intents that reached the same lines; the resolution follows the accepted outcome and the current source of truth, never the convenience of removing the marker. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Before editing, inspect the state of the operation: `git status`, which operation is active (merge, rebase, cherry-pick, revert), its base and both sides, every conflicted path, and any unrelated work in the tree that must survive untouched. For each hunk, trace both sides to their closest authority in this repository: the accepted specification or task, the canonical generated source (the committed `api/openapi/service.yaml` is regenerated, never merged by hand), the commit and pull request that introduced each side, the current callers, and the tests that observe the behavior. State both intents in plain words before choosing; a hunk whose two intents you cannot name is not ready to resolve.

Under read-only authorization, stop after the reconstruction and return the hunk dispositions, the proof plan, and the exact blocker without editing, staging, or continuing the operation. Under change authority, preserve compatible intents together. When the intents conflict, follow the accepted outcome and the current owner of that fact; never invent behavior to reconcile the sides, and never take one whole side of a file when the evidence is hunk-level. A conflict in `Cargo.lock` is resolved by taking the base and letting Cargo re-resolve with `cargo update --workspace` under `--locked` discipline, not by hand-merging entries. If authority cannot choose between the intents, leave the operation intact and return the exact decision blocker to its owner.

After resolving, inspect the whole diff from the operation base rather than the conflicted hunks alone, because a clean textual merge can still combine two changes that were never built together. Run the smallest proof that observes the affected behavior: the changed crates' tests through the affected-crate planner, the drift test when the contract or its annotations were touched, `make fmt-check` when formatting could have moved. Stage only the resolved paths and continue the already-authorized operation; do not start a new one or amend history the task did not authorize.

Return the operation and its base, the intent sources for each hunk, the dispositions, the proof that ran, and any unresolved blocker. Do not report a resolution as complete while a hunk remains whose intents were guessed, and do not expand the task into refactoring the merged code.
