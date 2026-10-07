# Goal

status: ready

Completion: R1-R4 are implemented and assembled at the accepted boundaries;
one delivery owner establishes matching local build/tests, required scoped
recovery/execution evidence and independent assembled source review, then
commits/pushes this branch and creates a new reviewable PR with successful
`required` and `codeql-required` for its actual head. The PR explicitly
dispositions #243 without modifying or closing it. Pending required evidence
remains incomplete; a local result alone does not complete delivery.

Global constraints: [Specification](spec.md), [System Design](design/system.md),
[Execution Design](design/execution.md), [Ownership Map](design/ownership.md),
[current Implementation](../../docs/spec-first-workflow/phases/implementation.md),
[Build Speed](../../docs/build-speed.md), and
[Validation Routing](../../docs/validation-routing.md) govern this ledger.
Only the bound Ledger Orchestrator writes this index during execution; Leads
write their packet's implementation details/results for relay to that owner.
No test-design phase, per-task check, self-review, review or acceptance gate.
Task checkboxes record integrated implementation only.

## Tasks

- [x] T1: Both process roots fail through their existing cleanup when armed readiness completion progress is lost.
  - Depends on: none; accepted H/S/W contracts are closed.
  - Provides: Serialized health lifecycle capability, service/worker integration, root-owned proving code and runtime guidance.
  - Packet: [T1](tasks/T1-process-progress.md)
- [x] T2: Existing real-database consumers establish recoverable useful work and instance-local isolation at the accepted HTTP/gRPC/topology boundary.
  - Depends on: none; current adapter contracts and accepted C composition are sufficient; T1 runtime code is not a fixture prerequisite.
  - Provides: Cohesive existing-runner consumer coverage, retained-profile projection and boundary-qualified guidance.
  - Packet: [T2](tasks/T2-consumer-recovery.md)
- [x] T3: Existing validation entrypoints retain exclusive generation custody until their own process and named external work are terminal.
  - Depends on: none; accepted E1 and L/D contracts are closed.
  - Provides: Safe lock admission, nested custody, visible bounded waiting, finite daemon tickets and complete entrypoint integration.
  - Packet: [T3](tasks/T3-validation-custody.md)
- [x] T4: Existing build/verifier entrypoints support truthful command-scoped compiler caching, resource decisions and compatible receipt reuse.
  - Depends on: T3 integrated implementation of generation custody and current Make/verifier integration; no passing receipt required.
  - Provides: B/V context helper, optional pinned cache mode, resource/receipt identity and portable documented entrypoints.
  - Packet: [T4](tasks/T4-build-context.md)

## Execution and ready frontier

Carrier: existing continuation coordinator binds as `LEDGER_ORCHESTRATOR`,
using the [Codex adapter](../../docs/agent-harness/codex.md). Dispatch fresh
Acceptance-Unit Leads through collaboration, with this checkout as their actual
root. Each Lead owns one packet and returns Implemented; the Orchestrator owns
serial integration and records its native locator here after dispatch. The
Planning actor stops at its reviewed transition.

Bound Orchestrator: `/root` (`LEDGER_ORCHESTRATOR`).
After the deliberate turn interruption, native state contains only the current
root; the previous Orchestrator and all descendants are absent. Resume retains
the existing ledger and integrated T1-T3 outputs. The current root owns only
routing/ledger updates; the fresh T4 Lead owns the remaining T4 implementation.
Current native execution:

| Unit | Lead | State and mutable ownership |
| --- | --- | --- |
| T1 | `/root/operational_continuation/t1_process_progress` | Implemented and integrated in the shared tree; Lead and S/W lane joined, T1 mutable scopes released. |
| T2 | `/root/operational_continuation/t2_consumer_recovery` | Implemented and integrated; Lead and profile/docs lane joined, shared metadata released. T1's three outbox marker IDs are registered. |
| T3 | `/root/operational_continuation/t3_validation_custody` | Implemented and integrated; Lead and both execution lanes joined, L/D/entrypoint/shared scopes released to T4. |
| T4 | `/root/t4_resume` | Implemented and integrated; ten-file candidate a0566ae03b5e79cbbdc9711d5e42fa6b71224c8b265537322414962d54e73675 matched root readback, no descendants, mutable scope released. |

Assembled Completion owner: `/root/operational_delivery`, fresh native
Acceptance-Unit Lead with Astra/xhigh. All four implementation scopes are
released. This owner alone selects/runs final proof, fixes the assembled
candidate, obtains independent Implementation Review, and delivers the new PR
and required actual-head CI. No final acceptance is recorded yet.

The native dispatches were accepted and returned these identities. Leads return
unverified implementation and joined writers, without committing or publishing.
The Orchestrator serializes any compile feedback commands; runtime validation
and independent source review remain at assembled Completion.

Current coding-feedback limitation: T1's filesystem preflight reported 702 MiB
free (100% used). No compile or runtime command had started for this outcome.
Compile admission is deferred pending a fresh capacity observation; both units
continue writing code/tests and inspecting source. No source, target or cache is
deleted to recover space. Unavailable diagnostics remain explicitly unverified
and do not become a per-task handoff gate.
The parent confirmed that, if this shortage persists, Completion may obtain
matching build/runtime evidence from required actual-head CI and retain the
local-unavailable limitation. Optional native cache provisioning/observation
must not consume the final hundreds of MiB; use a scoped CI observation when
needed. No new local environment is created solely to obtain completion.

Resume capacity observation: `df -h .` reported 6.7 GiB available after the
interruption. This replaces the current 702 MiB observation, but does not prove
sufficient capacity for a full build or native cache provisioning. Re-observe
capacity before the final delivery owner selects and executes its proof route.

Initial implementation frontier: T1, T2, T3. T1 is disjoint from T2/T3.
T2 and T3 share the existing profile/projection metadata owner and cannot mutate
it concurrently. Admit T1 and T2 first; release T2's shared owner at its joined
Implemented handoff and immediately admit T3 without waiting for tests/review.
This is exclusive-owner scheduling, not an invented behavioral dependency or
persisted wave. T4 becomes ready on integrated T3, subject to free shared owners.
If execution order changes, recompute owner availability without reslicing tasks.

The only shared mutable integration bundle is the existing template profile/
projection/classifier path family; T2, T3 and T4 take it serially. T3 then T4 also
share Make, verifier and command/delivery documentation. Each packet names its
exact semantic responsibility. A shared checkout needs no extra worktree for
these serial writes. Within T1, any H/S/W sublanes establish the narrow health
API first and integrate roots serially; within T3, L and D remain one coherent
custody outcome. Leads may choose useful disjoint lanes under Implementation,
not delegate an overlapping mutable owner back to themselves.

All CPU-heavy or compile feedback commands are serial, with this worktree's own
target and the current Git-common validation path. Coding uses bounded static
type diagnostics; command selection follows current owners. An unavailable
later runtime environment does not prevent supported coding. No task waits for
its own validation before unlocking dependent code.

## Completion ownership and gates

Integrated implementation results (not acceptance):

- T1: `Implemented`, base `699887b18594088a59bcc23a049d290d089f6da1`,
  nine-path diff SHA-256
  `d5e5047021015f8517a1355d90d5157af8bdc1adedb095607271ec476c2f5db5`.
  Orchestrator readback matches the [packet handoff](tasks/T1-process-progress.md).
  H/S/W code, owner-local/process tests and runtime guidance are present.
  Only rustfmt ran; compile/build/tests/review remain unverified. Native Lead
  and descendant are completed. T2 integrated the agreed three-marker metadata delta.
- T2: `Implemented`; the [packet handoff](tasks/T2-consumer-recovery.md)
  describes eight source/docs paths. Orchestrator readback matches all supplied
  critical file hashes. Its ordered eight-path `sha256  path` manifest is
  `dc3841a8b72168631451410c609ee7b73597f9de0ca693063a3e77e757371979`
  (Cargo.lock, test/Cargo.toml, postgres.rs, operational_recovery.rs,
  template_profiles.json, template_init.py, grpc.md, validation/postgres.md).
  Both native writers completed. Scoped rustfmt and offline locked Cargo
  metadata succeeded; compile/runtime/review remain unverified. Four existing
  dev edges and the three T1 markers are integrated without version changes.
- T3: `Implemented`; ordered 22-path manifest (21 source/docs paths and the
  [packet](tasks/T3-validation-custody.md)) SHA-256
  `53f4a041475da7e3f663f02a5d2dd99863ef609e7ee8cc2eea984e907c2ec90b`
  matches Orchestrator readback. All three native actors completed. Static
  Python/Bash/Make-body parsing passed; one isolated coding probe retained
  known exit 7 and released terminal custody. Runtime suites, lint, review and
  shared activation remain unverified. Unknown BuildKit outcomes and incomplete
  runtime-progress launch identity retain explicit reconciliation cost.

One Operational-recovery Delivery Lead owns final validation, independent source
review, repair coordination and external delivery; bind its native identity
only after all four units are Implemented and assembled and every writer/lane
has joined. It is an acceptance-owning Lead under Implementation, never an
execution-only worker or reviewer. The Orchestrator records its final verdict
without repeating validation. Repairs return to the relevant implementation
owner serially; retain proof unaffected by the repair.

Select one consolidated validation route from the actual assembled changes,
covering all packet claims. Run matching build/relevant tests once at that
boundary and collect scoped repairs only. Preserve accepted real PostgreSQL
useful-work, root lifecycle and lock/cache evidence at their existing owners;
combine observations covered by the same candidate/command. The classifier's
heavy image/database/migration/projection steps stay CI-owned, unless an
explicit local requirement already applies; do not invent a local full matrix.
Required database evidence may come from matching actual-head CI execution at
its canonical runner. An inherited #254/#255 pass cannot establish this change.
The cache observation remains explicit task-local provisioning and native stats,
with no inferred hit or speedup. Implementation selects the cases and commands
and retains their locators in its packets for the delivery owner.

The delivery owner fixes the assembled source candidate and obtains one fresh
independent [Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md)
covering H/S/W ordering, C claim scope, L/D custody and B/V identity/interactions.
Resolve blocking findings; apply shared Review's bounded repair rules. This is
source-delivery review, separate from upstream Specification/Design/Task Review.
Keep the consumed candidate stable during validation/review and join readers
before repair. No per-task review or duplicate acceptance stage is introduced.

Before first shared-path activation, use the candidate lock entrypoint for
this task's admissions and read-only establish absence/completion of already
admitted legacy work under [E1](design/execution.md#e1-kernel-exclusion-plus-generation-custody).
Contested legacy state prohibits shared activation and unrelated interruption;
it does not block isolated proof, source review, PR or required CI. Retain the
precise local limitation and route required claims to their existing admissible
proof boundary. Do not claim that two unmodified legacy reclaimers are repaired,
upgrade every checkout or prove absence of all future old callers.

After applicable local/source-review gates, load current contribution and
[External Effects](../../docs/spec-first-workflow/shared/external-effects.md)
owners, commit scoped source/artifacts, push `codex/operational-recovery-20261006`
and create a new PR against current main. Describe #254's adopted health bytes,
its stronger pool lifecycle superseding #243's pool hunks, and this PR's remaining
timing/topology guidance plus R1-R4. Attach the created PR to the current chat.
Read back PR head and both required gate conclusions at that SHA; a later head
requires its own applicable CI result. No force-push/closure of #243, main merge,
deployment, production reads, purchase, global machine/security/config/cache
mutation, cache deletion or unrelated runner interruption is authorized.

Record one replaceable Completion result here with candidate/commit, local and
CI evidence, review verdict, PR URL/head, any retained activation limitation and
remaining required scope. `done` requires successful requested delivery, not
four checked boxes. Genuine unavailable required proof keeps that scope
incomplete under parent-owned recovery; optional or expressly permitted shared-
activation limitations do not invent new gates.
