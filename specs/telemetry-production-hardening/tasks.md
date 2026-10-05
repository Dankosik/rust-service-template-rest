# Production telemetry hardening execution

status: ready

Completion: S1-S4 in [Specification](spec.md) implemented in one separate PR,
with the agreed behavior, matching build and relevant passing tests, no known
in-scope defect, and resolved final independent review. Applicable CI must pass
on the published PR head. Local acceptance and the requested PR/CI result are
reported separately until both are established.

Global constraints: [Intent](intent.md), [ready Design transition](technical-design-transition.md),
[Design](design/technical-design.md) and [ownership](design/ownership.md) remain
authoritative. No merge, deployment, infrastructure/backend experiments,
dependency/feature/manifest change, sampling or histogram change. The current
workflow and [validation budget](../../AGENTS.md#validation-budget) govern execution.
One assembled final validation boundary follows all code; no per-task test or
review gate and no concurrent heavy validation.

## Tasks

- [x] T1: Built-in inbound HTTP/gRPC observation emits only admitted request diagnostics.
  - Depends on: none.
  - Provides: independently usable source privacy and its operating delta; no shared telemetry API change.
  - Packet: [T1](tasks/T1-inbound-privacy.md).
- [x] T2: Shared telemetry and every existing consumer provide bounded, private output and truthful bounded termination.
  - Depends on: T1 implementation, consuming its admitted inbound source fields and documentation baseline for the complete egress contract; no passing receipt required.
  - Provides: complete shared capture/output/SDK-diagnostic/trace-drain behavior and consumer cleanup, with final operating contract.
  - Packet: [T2](tasks/T2-shared-telemetry.md).

## Execution boundary

Persisted state is needed because implementation, final validation and PR delivery
cross actors after Planning. The existing root binds as `LEDGER_ORCHESTRATOR`;
a fresh general-purpose Acceptance-Unit Lead implements each ready packet via
the native Codex harness. Related-unit Lead reuse is allowed by the ledger
contract after T1 is integrated and its writers stop. Leads can use worthwhile
disjoint subtask lanes; shared API declarations precede consumer use, writable
owners must not overlap and all descendants join before returning Implemented.
The root alone writes this ledger; the assigned delivery owner validates and
obtains final review once both units are assembled. Native identities are filled
on dispatch, never invented here.

T1 is separately consumable HTTP/gRPC request-observation privacy. All panic
emission/recovery stays in T2: changing only HTTP recovery cannot suppress the
shared hook that runs first. T2's formatter, queue, observation
state, guard and all entrypoint consumers form one resource-lifecycle outcome;
separating those layers would expose an unowned guard, unbounded cleanup or
misreported final state. The independent source change therefore gets its own
unit; internal implementation lanes do not become acceptance units.

## Completion result

Implementation is complete and assembled; final validation and acceptance are
pending. No build, behavioral-test, independent implementation-review or CI
pass is claimed. The final owner selected a workspace build and workspace tests
(planner fallback `outside_crates`), documentation consistency and one assembled
review, followed by applicable CI on the published PR head.

Documentation consistency passed after repairing one historical source locator
to the immutable baseline. Compile-only feedback stopped on an environmental
`ENOSPC` before our code diagnostics; its retry waits for another session's
shared validation lock. No shared cache or other worktree was deleted. Linux
process proof remains required in its actual execution environment, not inferred
from skipped macOS cases. Draft publication is authorized with these limits
explicit, while validation and repairs continue under the same delivery owner.

## Execution

T1 returned Implemented from `/root/telemetry_t1` (native Astra/high, fresh
history), with all writers stopped. Its three-file bounded diff was integrated
in the shared task worktree and its SHA256 verified:
`68e4e2baad56eb2af949132fe3fa87611563705022c55c664ef5af781e6c46d6`.
HTTP/gRPC source privacy, normalized methods and inline HTTP exported-boundary
fixtures are present but unverified. T2 consumes them immediately. Actual local
JSON/text and gRPC server/client output proof remains with assembled delivery.
The root is the sole ledger writer; final validation and publication remain
pending until T2 returns and its writers stop.

T2 is running under `/root/telemetry_t2` (native Astra/xhigh, fresh history),
with the integrated T1 baseline and its remaining shared-output proof scope.
Its disjoint output, formatter, traces, consumer, migration/panic and gRPC-proof
lanes remain under that Lead. A transient native model-capacity error interrupted
the Lead and consumer lane; the same Lead was resumed after reconciling the
native tree, preserving completed output/trace/migration work and active writers.
No duplicate lane, scope reset or aggregate validation was started by recovery.

T2 returned Implemented and all six native descendants are completed. The
assembled T1+T2 patch was integrated in the task worktree and its snapshot
SHA256 verified: `eb0c080e69ccf171af249061c632259edd5fe104c7dc538871181fb39d431059`.
Formatting/whitespace feedback passed; compile-only feedback did not execute
because the shared lock was occupied. No build, behavioral suite, final review
or CI evidence exists yet. All code writers are stopped; the root can assign
one final assembled delivery boundary now.

Final assembled validation/delivery is assigned to `/root/telemetry_t2`, reusing
the implementation Lead's current context. This is the single Completion
boundary for T1+T2; all descendants are stopped. The source candidate is frozen
for checks/review, subject only to owner-held validation repairs. CI publication
and exact-head results remain part of Completion.

The ledger's current Implemented/pending-Completion state is frozen for draft
publication. Publishing it does not claim acceptance or release readiness.
