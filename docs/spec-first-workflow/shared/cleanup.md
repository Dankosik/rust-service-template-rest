# Cleanup

Run at proven task closeout (or for explicitly scoped cleanup) to retire
task-owned temporary resources and execution-only artifacts.

- **Retire only task-owned resources:** remove temporary files, task-local build
  outputs, execution-only `tasks.md` / `tasks/`, and task worktrees once their
  consumers finish and needed commits and evidence are preserved. Leave shared
  caches, persistent data, and unrelated workspaces to their owners.
- **Worktree retirement:** follow [AGENTS.md](../../../AGENTS.md#branches-and-worktrees)
  and [Branches and Worktrees](../../worktrees.md#5-post-merge-closeout-and-stale-sweeps).
  Verify both inactivity (no live session, process, or lock) and commit
  preservation outside the checkout—a clean `git status` alone proves neither.
  If uncommitted/untracked files exist or ownership or recoverability is
  uncertain, leave the tree and report its locator and reason without reopening
  the completed outcome.
- **Artifact retirement:** promote any lasting rule, deciding commit/PR, and
  objective `Reopen if` trigger into its canonical doc (`docs/decisions/`,
  `docs/repo-architecture.md`, etc.), then delete the completed `specs/<task>/`
  bundle (retain it only while a live authority cites it). Run `just docs-check`
  and `just typos` if cleanup edits tracked docs. **Git is the archive**—never
  keep completed ledgers as a second history.
