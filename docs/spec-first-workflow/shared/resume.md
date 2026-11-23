# Resume

Use after compaction, interruption, or a session handoff. Verify live workspace
and artifact state directly instead of trusting a compaction summary.

1. **Inspect current ground truth:** check `git status`, `git diff`, active
   worktrees, and live native agent/task state. For Direct Work (no
   `specs/<task>/`), resume directly from those live changes.
2. **Route by exact `tasks.md` status:**
   - `status: done`: report the completed ledger.
   - `status: blocked`: inspect `Blocked stop:` and resolve it with the named
     owner first; once resolved, resume [Implementation](../phases/implementation.md)
     if `Base:` or `[x]` runtime state exists (or if the blocker occurred during
     Implementation), otherwise resume [Planning](../phases/planning.md).
   - `status: ready`: if `specs/<task>/design.md` exists at `status: draft`
     (mid-task steering reopen), re-clear [Design
     Review](../phases/design-review.md) before dispatching dependent tasks
     while unaffected tasks continue; resume
     [Implementation](../phases/implementation.md) from `tasks.md`, Git tips,
     and live native task state (a stored `Execution:` locator alone does not
     prove a task is still running).
   - `status: draft`: if `specs/<task>/design.md` exists at `status: draft`,
     resume [Technical Design](../phases/technical-design.md) first; otherwise
     resume [Planning](../phases/planning.md).
   - Before `tasks.md` exists:
     - if `specs/<task>/design.md` exists, resume [Technical
       Design](../phases/technical-design.md) when `status: draft` or continue
       into [Planning](../phases/planning.md) when `status: ready`;
     - otherwise, if `specs/<task>/spec.md` exists, confirm it is complete under
       [Specification](../phases/specification.md) and continue into [Technical
       Design](../phases/technical-design.md) (or [Planning](../phases/planning.md)
       if a lightweight inline `## Architecture` is already closed);
     - otherwise, if `specs/<task>/intent.md` exists, resume
       [Intake](../phases/intake.md) when `status: draft` or continue into
       [Specification](../phases/specification.md) when `status: ready`.
3. **Handle mid-task steering in place:** when new user guidance changes
   behavior, scope, or constraints mid-flight, update the affected clause in
   `spec.md`/`design.md` and impacted task packets in place (downgrading
   `design.md` to `status: draft` with `Design review: pending` and re-clearing
   [Design Review](../phases/design-review.md) before resuming dependent work
   whenever a `one-way door`, runtime crossing, `S<n>` seam, or rollout gate
   changes),
   notify affected live Leads one-to-one with the exact delta, invalidate only
   dependent work and proof, and let unaffected tasks continue.
