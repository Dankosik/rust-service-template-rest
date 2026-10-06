# Goal

status: blocked
Completion: B1–B6 in [spec.md](spec.md) are implemented and established on the assembled main-based candidate: usable admission, durable-effect adoption, controlled one-record DLQ recovery, actual bounded native recovery and R3/TLS capacity observations, three evidence-backed operating dispositions, and causal feedback repairs. One delivery owner completes matching validation and independent final review, then the root publishes a coherent reviewable PR with the actual applicable exact-candidate CI outcome. No merge or deployment.
Global constraints: [intent](intent.md#constraints), [design T3](design/system.md), [ownership T2](design/ownership.md), and [Technical Design transition](technical-design-transition.md) govern implementation. Only the bound Ledger Orchestrator writes this index after dispatch. Checkboxes mean integrated Implemented, never acceptance. All tasks include code/test-writing and their portable profile/documentation closure. No per-task test execution or review gate. Keep CPU-heavy work serialized, worktree build artifacts separate, and fixture limits distinct from build admission.

## Tasks

- [ ] T1: Startup admits only the supported durable, ACK-capable transfer path.
  - Depends on: none
  - Provides: Main-compatible #239 storage/ACK-loss integration and B1 admission.
  - Packet: [T1](tasks/T1-transfer-admission.md)
- [ ] T2: An adopter can compile and run the same durable-effect logic through the existing worker and admitted pool.
  - Depends on: none
  - Provides: B3 executable producer/effect example, example schema and deferred worker composition.
  - Packet: [T2](tasks/T2-durable-effect.md)
- [ ] T3: An operator can safely recover one exact DLQ record within an owned broker lifetime.
  - Depends on: T1 — admitted publication/custody behavior; no passing receipt required.
  - Provides: B4 operator example and reusable owned R3/TLS session runner with lifecycle fence.
  - Packet: [T3](tasks/T3-controlled-dlq.md)
- [ ] T4: Native restore/fault rehearsals account for every known logical identity and expose permitted recovery stops.
  - Depends on: T1 — admitted topology; T2 — producer/effect example; T3 — owned session and recovery entry.
  - Provides: B2 executable bounded recovery scenarios and identity evidence collection.
  - Packet: [T4](tasks/T4-native-rehearsal.md)
- [ ] T5: The real shared-worker R3/TLS path has reproducible capacity and failure-domain measurements.
  - Depends on: T1 — admitted topology; T2 — shared-pool producer/effect composition; T3 — owned R3/TLS session.
  - Provides: B5 bounded measurement capability; final observations decide publisher, MaxAckPending and roles.
  - Packet: [T5](tasks/T5-capacity-measurement.md)
- [ ] T6: The object-storage region-span instability has a causal repair or a proved existing resolution.
  - Depends on: none
  - Provides: B6 tracing capture/instrumentation correction with unchanged telemetry semantics.
  - Packet: [T6](tasks/T6-tracing-feedback.md)
- [ ] T7: Validation waiting is observable while the shared lock retains custody through child termination.
  - Depends on: none
  - Provides: B6 lock owner/wait/terminal diagnostics and conservative stale-owner behavior.
  - Packet: [T7](tasks/T7-validation-lock.md)

## Completion ownership and delivery

The root binds the sole Ledger Orchestrator and assigns one delivery owner before
final validation. Initial frontier: T1, T2, T6, T7, subject to packet locks and
native capacity. Refill immediately after integrated results. T4 and T5 have
no semantic dependency on each other; their shared fixture writer lock makes
their edits serial. No task is reserved for integration, test execution or review.

After every code task is Implemented, assembled and writer-free, the delivery
owner selects one non-overlapping validation plan from current repository
owners. It covers packet claims, the explicit B2/B5 actual observations, B3/B4
durable outcomes, B6 causal evidence, relevant profile/Go-wire gates and one
independent integrated review. Reuse one release build across fixture scenarios;
do not multiply full builds across harnesses or databases. Required external CI
is observed on the published candidate, not inferred from #239 or draft cheap
checks. Missing required observations leave Completion incomplete.

T5 initially retains publisher concurrency 1, effective broker-default
MaxAckPending and shared roles. Its final evidence may reopen only the
corresponding Technical Design choice before a runtime change. Any resulting
implementation repair returns to the responsible task owner, records the
accepted scope/locks here, and invalidates only affected final evidence. It
does not introduce a test-plan phase or repeat unrelated checks.

Selected delivery boundary: one main-based PR containing the coherent B1–B6
candidate and task evidence. T6/T7 remain independently extractable outcomes,
but extraction is not required and must not duplicate final validation. Preserve
main #254/#255; semantically incorporate unmerged #239 head
`7223ea877f031d440842d3df6876857e91492ec2`, never transplant its whole tree.
The root owns commit, publication and PR lifecycle under existing authority.

## Execution

Ledger Orchestrator: `/root`, bound on 2026-10-06 using the repository
orchestrator carrier and native Codex collaboration. This is the sole canonical
ledger writer. Leads share this task worktree with disjoint packet owners;
there is no source integration worktree or shared build target from another
checkout. Initial source locks are assigned to T1/T2/T6/T7. T7 owns the mutable
validation-lock implementation, so no heavy command may use it until that
writer returns. Rust static diagnostics are serialized and require separately
admitted build space; unavailable diagnostics do not block code handoff.

## Results

Implementation dispatch is blocked by unavailable native session storage.
Two attempts to start the fresh T1 Lead failed during thread-writer-lock
creation with `No space left on device`; neither returned an agent identity.
The native tree was reconciled after the first failure and has no implementation
Lead. No task has started, and no source/runtime/test change or Completion
verdict is claimed. Other phase actors and reviewers have ended.

The host data filesystem read back 116 MiB, then about 110 MiB free after the
failed dispatches. This worktree has no build target and only a 36 MiB generated
CodeGraph index. The prior task's clean PR #239 checkout has no target either;
only its ignored CodeGraph index is present (45 MiB whole checkout). No shared
cache, unrelated artifact, source, worktree or process was deleted or stopped.
The selected 2 GiB free-disk fixture floor is also unavailable, independently
of this session-creation failure. Existing CI remains a possible later proof
route; it cannot substitute for unavailable native implementation carriers.

Resume when native session storage can create a fresh Lead: reconcile this
checkout and the native tree, retain the reviewed upstream artifacts, then
dispatch T1/T2/T6/T7 under their existing locks. This is a capability gap, not
a new product or technical decision. Keep this blocked routing result until
that input changes; do not redispatch the same unavailable carrier blindly.
Publication of the prepared artifacts as a draft preserves the resumable work
and does not establish any B1–B6 implementation or CI/runtime guarantee.
