# Implementation

Use when accepted implementation work is ready and authorized. Leads implement
the planned units and their tests; one Delivery Owner validates the assembled
candidate at the end.

| Bound as | Follow |
| --- | --- |
| Ledger Orchestrator for a ready `tasks.md` | [Carrier](#carrier), then the [Orchestrator Loop](#orchestrator-loop) |
| Acceptance-Unit Lead for one unit, or a lane inside it | [Unit Implementation](implementation/unit.md) |
| Delivery Owner for the assembled candidate | [Final Validation](implementation/final-validation.md) |

## Carrier

In the session the user starts to execute a plan, open the named `tasks.md`
and check its current state under [Resume](../shared/resume.md). Report a
completed ledger as done and route an unready plan to its recorded owner.
For a ready ledger, bind this session as Ledger Orchestrator
(`LEDGER_ORCHESTRATOR`), including for a one-task ledger.

A fixed inline unit has no ledger. Its session is the root-local Lead: it
follows [Unit Implementation](implementation/unit.md), then acts as Delivery
Owner under [Final Validation](implementation/final-validation.md).

## Orchestrator Loop

The Orchestrator routes. Each unit's code, tests, and validation belong to its
Lead; the Orchestrator's own writes are the ledger and integration. [Ledger
Execution](implementation/ledger-execution.md) holds the rules each step
applies.

1. **Verify controls.** Confirm the selected [Agent
   Harness](../../agent-harness.md) adapter's full-ledger controls before any
   dispatch. Report a capability gap that prevents full-ledger execution.
2. **Dispatch the ready frontier.** Choose tasks from the [Ready
   Frontier](implementation/ledger-execution.md#ready-frontier) under
   [Dispatch](implementation/ledger-execution.md#dispatch) and dispatch one Lead
   per task. The brief names the packet and the ledger; the packet carries the
   outcome, writable scope, and locks. Record each Lead in the task's
   `Execution` field. This step is complete when no ready task with a disjoint
   scope waits while capacity is free.
3. **Consume each return.** Apply the [Acceptance
   Transition](implementation/ledger-execution.md#acceptance-transition): tie the
   result to its current assignment, integrate an `Implemented` change, record
   it, release its locks, and return to step 2. Route a `Blocked` result
   through [Parent-Owned
   Recovery](../shared/transition.md#parent-owned-recovery) while unaffected
   tasks continue.
4. **Start final validation.** When every task is checked and no writer is
   active, dispatch the Delivery Owner with the ledger path and the assembled
   candidate's identity. Route each repair request it sends to the owning
   task's Lead, and tell the Delivery Owner when that repair is integrated.
5. **Close.** Record the Delivery Owner's Completion Result. On `Accepted`,
   mark the ledger `done` and apply [Cleanup](../shared/cleanup.md).
