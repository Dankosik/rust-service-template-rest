# Planning Proof And Readiness

Read before declaring an inline plan or `tasks.md` ready.

## Final success

Keep the observable outcome with its owning task. Preserve explicit acceptance,
performance, migration, rollout, and external-effect requirements from accepted
behavior and design. Schedule execution of checks after the entire ledger's
code is implemented; do not create per-task proof or review pauses.

Planning does not enumerate test cases, design fixtures or fault controls,
approve assertions, or require exact test commands. The implementing agent
chooses those details alongside code and updates the existing packet with
commands needed by final validation. Absence of a test inventory is not a gap.
Known required release evidence or an already-specified check may be carried
without deriving a new test matrix.

Name canonical sources before generated output, accepted replacement/cleanup,
consumed code or contracts, mutable owners, and exclusive locks. Preserve real
external dependencies at their consuming action. Distinguish successful
Completion from a blocked stop; partial implementation is not verified behavior.

## Readiness dry run

Walk the next task through its implementation using accepted behavior, design,
current sources, and dependency timing under the [Ready
Frontier](ledger-contract.md#ready-frontier). This is a written walkthrough;
do not launch tests, services, or probes.

Confirm each existing item the task consumes, such as a function, type, trait
implementation, configuration key, or library API and its semantics, against
its source: current repository code, and for a crate, the source of the version
locked in `Cargo.lock`. A plan's or design's description of that item is not
evidence. A consumed item that is missing, or whose source contradicts the
planned use, leaves the task unready.

Ready means the executor can implement without inventing product behavior,
architecture, ownership, or authority, and the final observable outcome is
clear. Choosing a test technique is part of that implementation. An unavailable
future test environment does not block coding from closed inputs.

Reopen only an input needed to implement the accepted outcome. Do not persist
waves; the Orchestrator refills the ready frontier after implementation results,
and Leads choose useful parallel subtask lanes.
