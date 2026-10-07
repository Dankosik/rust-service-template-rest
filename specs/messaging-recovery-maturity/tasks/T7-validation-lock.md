# T7 — Visible validation waiting with retained child custody

Outcome:
A waiting developer can identify the lock owner and terminal state while only
one heavy command runs and the acquired lock remains held until its child ends.

Consumes:
- [B6](../spec.md#b6-trustworthy-feedback-during-development), [feedback design](../design/system.md#development-feedback-b6), [ownership F](../design/ownership.md#responsibilities).

Provides:
- Sanitized owner receipt, immediate and bounded wait feedback, acquisition/cancel/timeout distinction and conservative stale-owner refusal.

Boundary:
Extend the existing Git-common lock and self-test. Keep inherited ownership,
exact child exit status and child termination custody. Install waiting signal
handlers before sleep; timeout/cancel cannot run the requested command. Only
an acquired owner cleans up. Remove unsafe dead-wrapper-PID automatic reclaim;
stale/missing metadata is diagnostic, not evidence that all child work ended.
Document deliberate operator cleanup after process confirmation. No lock
manager, machine policy/cache changes or security exemptions.

Mutable owners:
- `scripts/ci/validation-lock.sh`, its existing self-test and `docs/build-speed.md` lock guidance.

Exclusive locks:
- Validation-lock implementation/self-test owner. Do not mutate it while any task is executing a heavy command under that implementation; the Orchestrator schedules that coding-feedback resource separately from source writers.

Final validation:
- Claim: B6 wait diagnostics are useful and serialization/cancellation/child custody remain correct.
- Checks: Existing extended lock self-test and matching changed-shell validation at assembled Completion; exact cases/commands stay executor-owned.
- Observable: Initial/10-second bounded wait messages identify readable/missing/stale ownership without secret arguments; acquire/cancel/timeout differ, unacquired command never runs, and live child custody cannot be reclaimed by stale wrapper metadata.

Reopen if:
Automatic reclamation becomes required or existing process custody cannot meet
the accepted conservative behavior; return that mechanism question to Technical
Design without weakening serialization.
