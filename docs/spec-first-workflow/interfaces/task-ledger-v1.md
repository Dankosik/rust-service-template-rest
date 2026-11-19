# Task Ledger V1

`specs/<task>/tasks.md` holds the task graph index, global completion target,
and active cross-task constraints. Detailed `Provides`, sources, boundaries,
locks, and validation claims live in each task's [Task Packet
V1](task-packet-v1.md).

## Authoring Schema (Planning-owned)

```markdown
# Goal: <short request outcome>
status: draft | ready | blocked | done
Blocked stop: <missing input/decision and owner; omit unless blocked>
Completion: <falsifiable observable condition for the assembled request, plus any requested external outcome>
Global constraints: <active rules, cross-task pinned shared identifiers, shared invariants, or carried CONCERNS; omit when none>

## Tasks

- [ ] T<ID>: <one-line observable outcome> [<prefactor | shared-contract | integration-first | expand | migrate | contract; omit for vertical slice>]
  - Depends on: <T<ID> [stage], <gate> [effect:<action>], or none>
  - Packet: tasks/<ID>-<slug>.md
```

- Order tasks **topologically** and by **dispatch priority** within each
  frontier (risk-retirement, `prefactor`, `shared-contract`, and critical-path
  blockers first).
- Start at `status: draft`; set `status: ready` after readiness review permits
  handoff, or `status: blocked` with `Blocked stop:` when preserving partial
  work alongside an upstream `blocked` handoff.
- **Stage syntax:**
  - Unannotated (`T1`) gates coding on `T1`'s integrated `Implemented` code —
    required whenever a task compiles against any Rust type, trait, function,
    config field, workspace dependency, exhaustive enum/struct change, or SQLx
    schema from `T1`, or an accepted sequencing constraint (such as an
    `integration-first` risk gate) orders `T1` first. Omit a coding edge only
    when the task compiles and unit-tests standalone against a wire, env, or
    already-landed contract (use `T1 [acceptance]` if assembly still requires
    both).
  - `[acceptance]` gates final validation (e.g., `T1 [acceptance]`);
    `[effect:<action>]` gates a named external action (e.g., `release approval
    [effect:release]`).
- Keep IDs, non-default shape tags, and stage tags identical in each packet
  (checked mechanically by `make ledger-check TASK=<task>`).
- `Global constraints` links accepted owners and pins cross-task shared
  identifiers once; update in place when guidance changes.

Orchestrator runtime state (`Base`, `Commit mode`, `Execution`, `Resume`,
`[x]`, and receipts) lives in [Ledger Runtime
State](acceptance-result-v1.md#ledger-runtime-state).
