# Branches and Worktrees

Operational runbook for parallel agent work in this repository.
[AGENTS.md](../AGENTS.md#branches-and-worktrees) owns the unconditional safety
boundaries; [Agent Harness](agent-harness.md) owns multi-unit delegation;
[Build Speed](build-speed.md#local-iteration-and-worktrees) owns Cargo `target/`
isolation and CoW cache warming;
[Cleanup](spec-first-workflow/shared/cleanup.md) owns task closeout.

## 1. Inspect Before Acting

Check where you are and what else is active before creating, syncing, or
removing any branch or worktree:

```bash
git rev-parse --show-toplevel
git rev-parse --abbrev-ref HEAD
git worktree list --porcelain   # or: wt list
```

- **Treat the primary `main` checkout as read-only for mutable tasks.** If
  `git rev-parse --abbrev-ref HEAD` is `main` and your task edits files (unless
  the user explicitly asked to edit `main` directly), do not write into the
  primary checkout; go straight to
  [Section 2](#2-create-anchored-to-an-explicit-base) to create a sibling task
  worktree anchored to `origin/main` and target all writes, checks, commits,
  and PRs inside that new path. Read-only inspection stays in place.
- **Detect an active linked worktree.** Compare
  `git rev-parse --path-format=absolute --git-dir` with
  `git rev-parse --path-format=absolute --git-common-dir`: when they differ
  (and `HEAD` is on a task branch, not `main`), you are already inside a linked
  task worktree (`--path-format=absolute` avoids false positives from
  subdirectories). Stay there; do not spawn another worktree unless delegating
  separate concurrent mutable sub-tasks under
  [Agent Harness](agent-harness.md) (always as sibling checkouts outside the
  repo root or via native harness isolation, never nested inside the current
  worktree).
- **Never mix control planes.** A worktree created by a harness
  (`claude --worktree`, Codex, Cursor, Antigravity) belongs to that harness; a
  worktree created by Worktrunk (`wt`) belongs to `wt`; a plain
  `git worktree add` belongs to Git. Retire each worktree with its own manager.

## 2. Create Anchored To An Explicit Base

Never switch branches inside a dirty or shared checkout, and never branch from
implicit `HEAD` (which may sit on an old or `:gone` branch). Refresh the target
ref first, then create an isolated worktree outside the repository root:

```bash
git fetch origin main

# With Worktrunk (`wt`):
wt switch -c <type>/<slug> --base origin/main --no-cd --format=json

# With plain Git (anchored to top-level root; `--no-track` avoids tracking `origin/main`):
root="$(git rev-parse --show-toplevel)"
git worktree add --no-track -b <type>/<slug> "${root}/../$(basename "${root}").<type>-<slug>" origin/main
```

- Use `<type>/<slug>` branch names (`feat/`, `fix/`, `refactor/`, `chore/`,
  `ci/`, `docs/`, `perf/`).
- Anchor to the explicit integration commit/ref named in the task brief when
  working on a feature integration branch instead of `origin/main`.
- **Worktrunk non-interactive and approval mechanics:** without shell
  integration in an agent subshell, `wt switch -c` creates the worktree without
  changing `$PWD`; pass `--no-cd --format=json` to read the created path and
  target it explicitly (or launch a command directly via `-x <cmd>`). Project
  hooks and aliases in `.config/wt.toml` require one-time user approval (`wt`
  prompts interactively on first use, or via `wt config approvals add`); agents
  must never self-approve them with `--yes` without explicit user authorization.
  When hooks are unapproved or skipped (or when `wt` is absent), invoke
  `wt step copy-ignored --require-include` directly on CoW filesystems (no
  approval required).
<!-- template:begin source-template:worktrees-harness-sync -->
- In source-template checkouts, generated harness files are gitignored. Once
  `.config/wt.toml` hooks are approved, `wt switch -c` runs
  `just agent-harness-sync` in `pre-start`; after plain `git worktree add` (or
  when `wt` hooks are unapproved/skipped), run `just agent-harness-sync` inside
  the new worktree so harness adapters and roles exist before starting work.
<!-- template:end source-template:worktrees-harness-sync -->

- **Recovering from a dirty or shared checkout (never ask the user):** never
  include, revert, or ask the user what to do with unrelated uncommitted files
  from another agent or session. If your edits already landed in a shared or
  `main` checkout alongside unrelated dirty files:
  1. Create a fresh sibling task worktree anchored to `origin/main` above.
  2. Copy or apply **only** the paths you modified into that new worktree and
     verify they are present there.
  3. Restore **only** your own paths in the shared checkout
     (`git checkout -- <your-paths>` for tracked files; remove only untracked
     files you created), leaving every unrelated path untouched.
  4. Continue validation, commit, and PR creation inside the new worktree.
  If you are already on your own task branch with unrelated dirty files present,
  stage strictly by explicit pathspec (`git add -- <your-paths>`).
- **Local `main` commit guardrail:** enable the repository's `pre-commit` hook
  once per clone (`git config core.hooksPath .githooks`, shared across all
  worktrees) to block accidental direct commits to `main` while still allowing
  fast-forward `sync-main` merges. Explicit overrides use
  `ALLOW_MAIN_COMMIT=1` or `git commit --no-verify`.

## 3. Isolate Mutable State

Git worktrees isolate files, index, and `HEAD`, while sharing `.git` metadata
and host resources. Keep per-worktree state disjoint:

- **Cargo `target/`.** Keep each worktree's `target/` separate and warm new
  worktrees on CoW filesystems per
  [Build Speed](build-speed.md#local-iteration-and-worktrees).
- **Untracked files and secrets.** `.worktreeinclude` deliberately excludes
  `.env*` so live credentials never copy silently across parallel agent
  worktrees.
- **Ports, containers, and databases.** Parallel local servers or integration
  tests must use ephemeral ports (or Worktrunk's `{{ branch | hash_port }}`) and
  unique `COMPOSE_PROJECT_NAME` / database names so concurrent runs do not
  collide.

## 4. Detect Drift, Sync, And Integrate Without Checking Out `main`

- **Never run `git stash`.** `refs/stash` is global to `.git` and leaks patches
  between parallel worktrees. Save in-progress work as commits on your own task
  branch.
- **Never check out `main` in a secondary worktree.** Git forbids checking out a
  branch already active in another worktree. Compare and update directly against
  the fetched remote-tracking ref (`origin/main`).
- **Check drift and overlap at natural checkpoints.** Do not interrupt mid-edit
  on every file save; check drift when starting a branch, after a pause/handoff
  or long (>30 min) pass, and **always** before final validation, PR creation,
  or merge:

  ```bash
  git fetch origin main
  git rev-list --left-right --count origin/main...HEAD
  git diff --name-only origin/main...HEAD
  ```

  React by overlap severity (only the owning agent/session of that worktree
  syncs its own branch):

  | Drift status | Meaning | Action |
  | --- | --- | --- |
  | `disjoint` | `origin/main` touched unrelated files/crates | Finish the current slice; sync once before final validation, PR, or merge |
  | `shared-or-same-crate` | `origin/main` touched shared manifests/contracts (`Cargo.toml`, `Cargo.lock`, `migrations/`, `api/`, `.sqlx/`, `justfile`, `mise.toml`, `AGENTS.md`, or `config`/`health`/`contract`/`shared-rules` crates declared in `quality/architecture.json`) or another file in a crate you are editing | Sync at the next clean commit before building further on stale contracts |
  | `direct-overlap` | `origin/main` modified files already edited or untracked in this worktree | Stop expanding the diff and sync immediately |

- **Choose `rebase` vs. `merge` by branch visibility:**
  - *Unpushed, private single-agent branch:* `git rebase origin/main` for linear
    history.
  - *Already-pushed PR branch or shared branch:* `git merge origin/main`
    (additive; preserves PR review anchors and avoids force-push). Use
    `git rebase origin/main` + `git push --force-with-lease` only when linear
    history is explicitly requested on an unshared branch and remote push is
    authorized ([Authority](../AGENTS.md#authority)); never use bare `--force`.
- **Resolve conflicts by intent, regenerate derived files, and re-verify:**
  - On textual code conflicts, load `.agents/skills/merge-conflict-resolution/SKILL.md`
    to reconstruct both sides' intent instead of blindly choosing `--ours` or
    `--theirs`.
  - Never hand-merge conflict markers in `Cargo.lock`, `api/openapi/service.yaml`,
    `crates/grpc-contracts`, or `.sqlx/`; resolve the canonical source first and
    regenerate via Cargo / `just` (see [Validation Routing](validation-routing.md)).
  - A clean text merge does not prove semantic compatibility: treat prior local
    test passes as stale after any non-trivial sync and re-run affected
    [Validation budget](../AGENTS.md#validation-budget) checks on the synced tree.
- **Keep local `main` fresh after every PR merge.** Immediately after a PR
  merges into `origin/main`, update both `origin/main` and local `main` (so
  `wt list`, `wt switch`, and local diffs stay accurate) using strict
  fast-forward only—never `git fetch origin +main:main` or `git branch -f main`,
  and never modify a dirty `main` checkout:

  ```bash
  git fetch origin main
  # If `main` is not checked out anywhere:
  git fetch . origin/main:main
  # If `main` is checked out in a clean primary worktree:
  git -C <main-worktree> merge --ff-only origin/main
  ```

- **Integrate serially and frequently.** Land completed branches one at a time
  onto the integration tip instead of letting parallel branches diverge for
  days. Avoid concurrent edits to shared generated files (`Cargo.lock`,
  `api/openapi/service.yaml`, `.sqlx/`) across active branches; regenerate once
  after integration.

## 5. Post-Merge Closeout And Stale Sweeps

A merged PR is not finished until local `main` is updated and the merged task
worktree and local branch are retired. Follow
[Cleanup](spec-first-workflow/shared/cleanup.md):

1. **Merge without `--delete-branch` inside a linked worktree:** run
   `gh pr merge <PR> --squash` (this repository enables GitHub's
   `deleteBranchOnMerge`, so GitHub deletes the remote branch without `gh`
   switching the temporary worktree onto `main`).
2. **Refresh local `main` from outside the task worktree** using strict
   fast-forward (`git fetch origin main` + `git -C <main-worktree> merge --ff-only origin/main` when clean).
3. **Confirm the checkout is clean and idle:** `git status --short` is empty,
   and no live session, background process, or build lock uses the checkout.
4. **Retire the merged worktree and local branch immediately—never `rm -rf`:**
   - **Worktrunk (handles both regular and squash merges):** run from outside
     the worktree:
     ```bash
     wt remove --foreground <branch>
     ```
     `--foreground` blocks so you can verify removal; `wt remove` refuses dirty
     trees without `-f` and deletes the local branch when its commits or
     squash-merged content are integrated into `main`.
   - **Plain Git:** run from outside the worktree once the merge is verified:
     ```bash
     git worktree remove <worktree-path>
     git branch -d <branch>   # or -D only after verifying the squash-merged PR
     ```
   - **Harness-managed worktree:** use the harness's retirement operation.

**Periodic stale sweeps are one-way doors.** To preview older merged worktrees
and branches across the repository (including squash-merged branches), run:

```bash
wt step prune --dry-run
```

`wt step prune` skips locked worktrees, dirty worktrees, the primary checkout,
and candidates younger than `--min-age` (default `1d`). Present the exact list
and obtain explicit user authorization ([Authority](../AGENTS.md#authority))
before running `wt step prune --foreground`, bulk-deleting `[origin/...: gone]`
local branches, or force-deleting unmerged branches. `git worktree prune` clears
only metadata whose directory is already gone; run it only after confirming no
live tool is recreating that path.
