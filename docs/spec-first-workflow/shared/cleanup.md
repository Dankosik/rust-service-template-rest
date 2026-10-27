# Cleanup

Use at proven task closeout when execution-only artifacts or task-owned
temporary resources remain, or for explicitly scoped cleanup.

Within the accepted effect authority, retire resources created for the task
after their consumers finish and required results, evidence, and recovery
material are retained. This includes temporary files, task-local build outputs,
execution-only `tasks.md`, `tasks/`, `workflow-plan.md`, and task worktrees.
Shared caches, persistent data, and unrelated workspaces retain their own
owners and cleanup authority.

A worktree is removed only when it is clean and no live session, process,
validation lock, or pending consumer still needs it. Preserve needed commits
through durable refs outside the checkout or the owning harness's recoverable
archive before retirement; a clean status alone proves neither inactivity nor
recoverability. Uncommitted or untracked paths stop that removal: name them
and leave the tree. Closeout does not invent a clean tree.

Use the owning harness or worktree manager's supported retirement operation
and verify its result rather than deleting a managed checkout as an ordinary
directory. If ownership, inactivity, recoverability, or authority is uncertain,
leave the affected resource and report its locator and retention reason;
unrelated cleanup does not reopen an otherwise completed outcome.

Retain a completed spec, design, research note, or rollout record only while a
live authority names it as a durable decision source. Otherwise move the
durable rule, deciding commit or pull request when provenance matters, and
objective reopen condition to its canonical owner, then delete the completed
bundle.

Git is the archive. Do not keep completed ledgers as a second history.
