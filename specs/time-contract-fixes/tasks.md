# Preserve time limits and authentication validity

status: ready

Execution owner: root `LEDGER_ORCHESTRATOR` in the isolated
`codex/time-contract-fixes-20261005` worktree. Native Acceptance-Unit Leads:
T1 `/root/time_fix_t1`, T2 `/root/time_fix_t2`, T3 `/root/time_fix_t3`,
T4 `/root/time_fix_t4`. All four units are Implemented and writers released.
The final T1 finding exposed an impossible agent-authored conjunction of
physical return and irreversible synchronous observation. Reviewed Definition
and Design reopen fixed the operation-decision boundary before terminal
observation; T1 documentation and its boundary proof now match it. Source
guards remain intact. Formatting and documentation evidence predates that
bounded delta; compiler/build/tests and a fresh final review remain incomplete.

Completion: All four core outcomes below are implemented and assembled; one
delivery owner establishes the matching build, relevant passing tests,
documentation consistency and resolved final authorization-sensitive review
under [Validation budget](../../AGENTS.md#validation-budget). This is local
core acceptance. The root retains the requested separate PR and its CI result
as external delivery obligations, and must reconcile the deferred webhook
horizon before fixing the overall PR scope. No merge or deployment is included.

Global constraints: [Intent](intent.md), [specification](spec.md), and
[technical design](design/technical-design.md) own behavior and mechanism.
The root is the sole canonical ledger writer during Implementation. Leads own
their packets and return implementation results to the root; packet edits do
not change ledger status. All four tasks are initially independent. Keep
writers disjoint; reconcile a discovered overlap before either writer mutates
it. Choose tests while implementing, then run CPU-heavy validation serially
only after all planned code is assembled and writers have stopped. Final
validation uses the repository planner to select the matching existing build,
test and documentation route, plus one assembled review. CI-owned heavy gates
stay in CI; do not create infrastructure, duplicate validation per task or
introduce dependency, toolchain, configuration, budget or identity changes.
See [ledger execution](../../docs/spec-first-workflow/phases/planning/ledger-contract.md).

## Tasks

- [x] T1: Outbound execution preserves one absolute end through dispatch and buffered completion.
  - Depends on: none.
  - Provides: Fixed-deadline behavior and its owning outbound documentation.
  - Packet: [T1](tasks/T1-outbound-deadline.md).
- [x] T2: Bearer verification uses usable current calendar time and preserves fresh versus retained introspection evidence.
  - Depends on: none; the accepted bounded HTTP contract is sufficient for implementation, and T1 assembles with T2 at Completion.
  - Provides: Fail-closed clock handling and calendar-safe introspection reuse with existing fresh semantics.
  - Packet: [T2](tasks/T2-authentication-time.md).
- [x] T3: HTTP Retry-After renders the exact whole-second ceiling across the complete Duration range.
  - Depends on: none.
  - Provides: A lower-bound retry hint that never rounds below its supplied delay.
  - Packet: [T3](tasks/T3-retry-after.md).
- [x] T4: Cache SET rejects inadmissible Redis TTL arguments through a typed caller error before dispatch.
  - Depends on: none.
  - Provides: SetError, admitted TTL conversion, compatible in-scope callers and cache guidance.
  - Packet: [T4](tasks/T4-cache-ttl.md).
