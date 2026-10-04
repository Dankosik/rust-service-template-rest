# PostgreSQL pool resilience implementation

status: done

Completion: The assembled local candidate provides bounded native SQLx return,
truthful acquisition diagnostics at the named operations, and usable connection
budget/sizing guidance. Matching local build/tests and the accepted bounded
real-database cancellation, acquisition and overload/readiness recovery proof
pass, source/profile custody is established, and final independent review has
no blocking findings. This establishes local completion only; unexecuted
CI/image gates remain explicitly pending CI.

Global constraints: [Intent](intent.md), [Specification](spec.md),
[reviewed design](design/design.md), [ownership](design/ownership.md), and
[dependency custody](design/dependency-custody.md) are the accepted inputs.
The [Planning transition](planning-transition.md) fixes their identities and
the execution carrier. One candidate remains in
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-postgres-pool-resilience-20261004`,
base/workflow revision `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Only the bound Ledger Orchestrator writes this index after Planning.

Ledger Orchestrator: `/root` (native Codex collaboration carrier).

## Tasks

- [x] T1: Cancelled or silent returned connections release local capacity within the native five-second return bound, with portable and removable dependency custody.
  - Depends on: none; source and ownership decisions are closed in Design.
  - Provides: Native SQLx backport, complete source/delivery/profile integration, runtime documentation and authored behavioral regressions.
  - Packet: [T1 — bounded native return](tasks/T1-bounded-native-return.md)
- [x] T2: Operators can distinguish acquisition pressure from execution failures at every named adapter operation, with the existing readiness policy recoverable after saturation.
  - Depends on: none for implementation; the fixed native SQLx contract and observation interface are supplied by Design. Assembled T1 is consumed by the combined cancellation/recovery claim at final acceptance.
  - Provides: Ordinary acquisition observer, named caller wiring, coverage documentation and authored acquisition/saturation recovery proof.
  - Packet: [T2 — acquisition diagnostics](tasks/T2-acquisition-diagnostics.md)
- [x] T3: Operators can calculate a complete connection allowance and select workload-specific sizing observations without invalid worker settings or a claimed production optimum.
  - Depends on: none; accepted budgets, worker minima and deployment contracts are available. Final acceptance checks consistency with assembled T1/T2.
  - Provides: Complete direct and PgBouncer budgeting/sizing guidance with a valid illustrative allocation.
  - Packet: [T3 — operating guidance](tasks/T3-operating-guidance.md)

## Execution and Completion boundary

The atomicity check retains three independently useful outcomes: the return fix
can serve native SQLx users without the new diagnostic helper; diagnostics can
serve the existing native pool without the return patch; sizing guidance can
be used without either code change. Source plumbing is part of T1, caller wiring
part of T2, and neither is split into a layer-only packet. R4 is accepted local
evidence, assigned to T2's authored saturation/recovery coverage and the final
assembled proof; running checks is not a fourth task.

The initial execution order is T1, T2, T3. This is serial shared-file custody,
not a proof dependency: the primary PostgreSQL test owner and Persistence guide
are shared. One Acceptance-Unit Lead may be reused after its prior unit is
integrated and writers stop. The Orchestrator may release disjoint implementation
lanes when their actual scopes and locks are separated, but must not permit two
writers to either shared file. No test or review receipt is needed to move to
the next unit. Implementation chooses cases, fixtures, commands and useful
lanes; it records the final commands in the existing packets.

After all three units are Implemented and assembled, with no active writer,
the existing delivery Lead owns one final-validation boundary under
[Implementation](../../docs/spec-first-workflow/phases/implementation.md#final-validation)
and the [Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md).
It consolidates the matching build/unit tests for the manifest and several-crate
surface, mixed-surface source/profile/dependency/docs proof and the explicitly
accepted local database observations. Reuse adequate current evidence; no
per-task checks, multiplied profile/database matrices or full-repository claim.
The selected owner has sole use of the shared validation lock and existing
database fixture during execution; no concurrent CPU-heavy validation.

Final independent Implementation Review is required once on that assembled
candidate because native return/cancellation changes concurrency safety and
interacts with transaction finality. Repair stays with the responsible existing
unit, with only invalidated proof repeated. The Orchestrator records Completion
without repeating proof or review. Missing required proof is verification
incomplete, not success. Existing image/initializer/security and other CI gates
remain selected and retain their own external-action scope. No push, remote
write, release, deployment, spending or production-optimal sizing is authorized.

## Results

Implemented: T1; verification: pending final validation; candidate:
`945caa8487d313026f75314bccd34b3f4e60654e7fc07752f35bfcf72257e166`
(123-file manifest `/tmp/pool-resilience-T1-implemented.json`, verified on receipt).
T1 includes the narrow shared-fixture enum consumer repair in
`test/tests/jobs/execution.rs`; its writers/readers have stopped.

Implemented: T2; verification: pending final validation; candidate:
`def2e61e6581be0a2fc3dc113a4fb82269c9b727e814d8149ce7810a45e84863`
(16-file manifest `/tmp/postgres-pool-t2-candidate.json`, all file/base hashes
verified on receipt). Its intentional `integration-tests -> tracing` dev edge
is separate from T1's source-only resolution. Writers/readers have stopped.

Implemented: T3; verification: pending final validation; candidate:
`dac4ba67d093d47def9a2ea66ac94babda8b857d27fa181207fa5078b2cd32a9`
(three-file manifest `/tmp/pool-resilience-T3-implemented.json`, verified on receipt).
Accepted: Completion; candidate `8643ccb74681dd9bb9c0694ef692d87c159dc1eb`.
Matching build, 804 unit tests and 34 real PostgreSQL checks passed; the
unpatched negative control failed at the intended slot-retention assertion,
and the restored patch passed. Fresh final independent review: PASS, no findings.
The proved source tree is `e114d7998992419a67e258144a6ef9b4c7fc8b96`;
the candidate adds only Completion/review receipts. Every writer, checker,
reviewer and task Compose resource has stopped. CI/runtime-image/full-initializer
gates remain pending. No main-checkout integration or remote effect occurred.

Closeout: archive execution-only artifacts through Git and preserve the tested
local branch. The main checkout remains under unrelated active hotpath work.
Verification and acceptance remain pending the assembled Completion boundary.
