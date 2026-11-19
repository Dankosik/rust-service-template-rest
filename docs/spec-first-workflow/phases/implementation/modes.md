# Execution Modes

Load only when executing in **`no-commit` mode** (the user explicitly forbade
local commits) or **inline unit mode** (a fixed unit with no `tasks.md` ledger).
Standard ledger runs commit each unit on the integration branch and skip this
file.

## No-Commit Mode

When the user forbade local commits, the Orchestrator applies these overrides
and passes the relevant active lines into each brief so downstream actors do
not re-derive them:

- **Pinning and serialization:** record `Commit mode: no-commit` in `tasks.md`,
  serialize every unit and repair in the shared checkout, and track
  `landed paths` across completed tasks and repairs.
- **Unit and repair landing:** verify changed paths stay inside granted scope
  (excluding `specs/<task>/` and prior `landed paths`), run
  `git add -N -- <newly-created-untracked-paths>` strictly on newly created
  paths (never deleted paths or `specs/<task>/`) so `git diff <Base>` sees new
  files without committing, and append all verified changed paths from the unit
  or repair to `landed paths`.
- **Delivery Owner read-only snapshot:** before dispatching **or resuming**
  Delivery Owner (refreshing immediately after every landed repair), capture a
  snapshot outside the repo (`git diff <Base>` saved to a temp patch plus the
  untracked path list outside `specs/<task>/`). On return, confirm zero delta
  against that snapshot; if Delivery Owner mutated candidate files, revert
  newly dirtied untouched paths and remove stray untracked paths then
  reject/re-run validation, or fail closed (`Blocked`, preserving the tree and
  naming the unauthorized delta) if any already-landed uncommitted file was
  modified — never blindly discard landed edits. Remove the temp snapshot at
  closeout.
- **Diff commands and repair resume:** substitute `git diff <Base>` everywhere
  `<Base>..HEAD` appears, and `git diff <Base> -- <repaired paths>` everywhere
  `<old-HEAD>..<new-HEAD>` appears. After a Repair Lead returns `Implemented`,
  land via `git add -N` above, refresh the pre-dispatch snapshot, and resume
  Delivery Owner with `git diff <Base> -- <repaired paths>`, whether scope
  expanded, and repair `decisions` (no `new HEAD`).
- **Brief fields:**
  - *Unit Lead and Repair Lead briefs:* include `Commit mode: no-commit` and
    prior `landed paths` (excluded from Step 3 scope checks).
  - *Delivery Owner brief:* include `Commit mode: no-commit`, pinned diff
    command `git diff <Base>`, and on Round 2 author `Repair Request`
    `prior_attempt` with Round 1's repaired paths and summary instead of a
    commit SHA.
- **Closeout:** if [Cleanup](../../shared/cleanup.md) updates tracked `docs/`,
  run `make docs-check` and `make typos` and leave the edits uncommitted.

## Inline Unit Mode

When executing a fixed inline unit with no `tasks.md` ledger, this session acts
as the root-local Lead (`ACCEPTANCE_UNIT_LEAD`): pin `Base` before the first
edit, run [Unit Implementation](unit.md), then proceed directly to [Final
Validation](final-validation.md) without an Orchestrator or `Implementation
Result` handoff.

| Boundary | Root-local inline delta |
| --- | --- |
| Candidate diff when uncommitted | Run `git add -N` on newly created paths and pass `git diff <Base>` (and `git diff <Base> -- <repaired paths>` after repair) to Final Validation and review instead of `<Base>..HEAD`. |
| Scope expansion or Cargo crate/feature | Decide directly within session authority (applying `rust-dependencies` and `AGENTS.md#authority` for dependencies). |
| Invalid behavior, architecture, or zero delta | Reopen directly under [Transition](../../shared/transition.md). |
| Final Validation `NeedsRepair` or refuted findings | Edit directly under [Repair Assignment](repair.md) (or record the refuting anchor) and return directly to [Final Validation Step 4](final-validation.md#4-verify-findings-classify-and-route-repairs). |
