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
   - `status: ready`: resume [Implementation](../phases/implementation.md) from
     `tasks.md`, Git tips, and live native task state (a stored `Execution:`
     locator alone does not prove a task is still running).
   - `status: draft`: resume [Planning](../phases/planning.md).
   - Before `tasks.md` exists: if `specs/<task>/spec.md` exists (with optional
     `design.md`), confirm it is complete under [Spec And
     Design](../phases/spec-and-design.md) and continue into
     [Planning](../phases/planning.md); otherwise, if `specs/<task>/intent.md`
     exists, resume [Intake](../phases/intake.md) when `status: draft` or
     continue into [Spec And Design](../phases/spec-and-design.md) when
     `status: ready`.
3. **Handle mid-task steering in place:** when new user guidance changes
   behavior, scope, or constraints mid-flight, update the affected clause in
   `spec.md`/`design.md` and impacted task packets in place, notify affected
   live Leads one-to-one with the exact delta, invalidate only dependent work
   and proof, and let unaffected tasks continue.
