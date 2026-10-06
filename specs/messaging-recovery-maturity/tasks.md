# Goal

status: ready
Completion: B1–B6 in [spec.md](spec.md) are implemented and established on the assembled main-based candidate: usable admission, durable-effect adoption, controlled one-record DLQ recovery, actual bounded native recovery and R3/TLS capacity observations, three evidence-backed operating dispositions, and causal feedback repairs. One delivery owner completes matching validation and independent final review, then the root publishes a coherent reviewable PR with the actual applicable exact-candidate CI outcome. No merge or deployment.
Global constraints: [intent](intent.md#constraints), [design T3](design/system.md), [ownership T2](design/ownership.md), and [Technical Design transition](technical-design-transition.md) govern implementation. Only the bound Ledger Orchestrator writes this index after dispatch. Checkboxes mean integrated Implemented, never acceptance. All tasks include code/test-writing and their portable profile/documentation closure. No per-task test execution or review gate. Keep CPU-heavy work serialized, worktree build artifacts separate, and fixture limits distinct from build admission.

## Tasks

- [x] T1: Startup admits only the supported durable, ACK-capable transfer path.
  - Depends on: none
  - Provides: Main-compatible #239 storage/ACK-loss integration and B1 admission.
  - Packet: [T1](tasks/T1-transfer-admission.md)
- [x] T2: An adopter can compile and run the same durable-effect logic through the existing worker and admitted pool.
  - Depends on: none
  - Provides: B3 executable producer/effect example, example schema and deferred worker composition.
  - Packet: [T2](tasks/T2-durable-effect.md)
- [x] T3: An operator can safely recover one exact DLQ record within an owned broker lifetime.
  - Depends on: T1 — admitted publication/custody behavior; no passing receipt required.
  - Provides: B4 operator example and reusable owned R3/TLS session runner with lifecycle fence.
  - Packet: [T3](tasks/T3-controlled-dlq.md)
- [x] T4: Native restore/fault rehearsals account for every known logical identity and expose permitted recovery stops.
  - Depends on: T1 — admitted topology; T2 — producer/effect example; T3 — owned session and recovery entry.
  - Provides: B2 executable bounded recovery scenarios and identity evidence collection.
  - Packet: [T4](tasks/T4-native-rehearsal.md)
- [x] T5: The real shared-worker R3/TLS path has reproducible capacity and failure-domain measurements.
  - Depends on: T1 — admitted topology; T2 — shared-pool producer/effect composition; T3 — owned R3/TLS session.
  - Provides: B5 bounded measurement capability; final observations decide publisher, MaxAckPending and roles.
  - Packet: [T5](tasks/T5-capacity-measurement.md)
- [x] T6: The object-storage region-span instability has a causal repair or a proved existing resolution.
  - Depends on: none
  - Provides: B6 tracing capture/instrumentation correction with unchanged telemetry semantics.
  - Packet: [T6](tasks/T6-tracing-feedback.md)
- [x] T7: Validation waiting is observable while the shared lock retains custody through child termination.
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
validation-lock implementation only during its task; that writer has returned.
After the user-requested continuation, the native old agent tree is empty and
no heavy child process is observed. T4's preserved partial source is assigned to
replacement `/root/t4_recovery_resume`; it has returned with joined writers.
T5 has returned Implemented with joined writers. All seven code tasks are
assembled and writer-free; the next owner is one final delivery Lead.
Rust static diagnostics remain serialized and require admitted build space;
unavailable diagnostics do not block code handoff.

## Results

Native session storage recovered after a fresh filesystem observation showed
744 MiB free. The initial two ENOSPC dispatches created no agents; after the
changed input, all four initial Leads started successfully. Five implementation
results are integrated; T4/T5 are also Implemented and no Completion verdict has
returned. The published preparation candidate is
`e20164b3a6430e8ddd416e2f6235105c95d2bf1d` in draft PR #257; that head contains
only reviewed upstream artifacts and the prior resumable resource stop.

| Task | Native Lead | Current state |
| --- | --- | --- |
| T1 | `/root/t1_admission` | Implemented; writers joined, owned sources released |
| T2 | `/root/t2_effect` | Implemented; writers joined, full identity reconciled |
| T3 | `/root/t3_dlq` | Implemented; writers joined and full identity reconciled |
| T4 | `/root/t4_recovery_resume` | Implemented; joined writers and16-file identity reconciled |
| T5 | `/root/t5_capacity` | Implemented; joined writers and11-file identity reconciled |
| T6 | `/root/t6_tracing` | Implemented; causal feedback complete, writers/processes joined |
| T7 | `/root/t7_lock` | Implemented; writers joined and mutable lock resource released |

Free disk now reads6.7 GiB. One coherent compile-only diagnostic can use the
current worktree target after parent confirmation: at most2 GiB extra output,
stop below3 GiB free, at most10 minutes with progress checkpoints, no debug or
incremental output, shared validation lock and locked Cargo resolution. No
runtime/test/service/fixture run starts before full assembly. Existing CI remains
the selected assembled final-proof route. T6's earlier bounded3.58 MiB causal
probe is complete; it creates no per-task acceptance gate.
No shared cache, unrelated artifact, worktree, source or process was deleted
or stopped to recover space.

T7 returned Implemented with joined writers. Receiving hashes matched:
`scripts/ci/validation-lock.sh` `46b89aa32ff44338c1071c50ed1e78f785160c2b4d46e74846feb528059d858b`;
`docs/build-speed.md` `712e3fc4016a38735aa26502a63d7a4603c44c12415f03563eb93b3ad920738b`.
Only Bash syntax feedback passed; behavior/ShellCheck/docs evidence remains
for assembled Completion. Canonical source is integrated in this shared tree.

T1 returned Implemented with joined writers; the eight-file sorted
`path NUL content NUL` hash reconciled as
`b51049a7cfd8212a4ea7500c78105486e91b2ccdba915187f83c316b719b0aa4`.
These sources are available to T3; no compile/runtime or review pass is claimed.

T6's admitted causal probe completed in 3.435 seconds with 3,758,060 bytes
retained. The gated first-registration schedule disabled the real tracing span
and failed the Amazon callback assertion (expected exit 101); a fresh process
passed the same assertions (exit 0). This demonstrates the failure path while
historical CI attribution remains unproved. All owned processes joined; CPU
slot is released. T6 now implements the scoped capture repair. Ordinary
source/real-store validation still waits for assembled Completion.

T6 returned Implemented; receiving Git blobs matched `tests.rs`
`566120cf4e4f17d585d76246dab267e8a58bed30` and tracing research
`70e4fd71fb8e3d78b7f00dd3ea6c2e71cfe0d143`. Two bounded exact-filter
subprocess capture tests preserve the telemetry assertions and now require
one operation span per provider. No production/manifest/dependency changed;
actual crate build/tests and final review remain Completion obligations.

T2 returned Implemented with14 joined-owner files. The sorted
`path NUL u64-big-endian-length content` SHA-256 reconciled as
`1c979c53cc980b9d791b72edd6b5f4b7659e3c02f30a6e9dbac3d1c47551ad26`.
Its shared profile/inventory/classifier contract is released to T3. The opt-in
worker composes ordinary `test.probe`, outbox and typed effect on the single
admitted pool (minimum6 with one ordinary slot); producer and fixture interfaces
are available to T4/T5. No compile/test/runtime result is claimed.

T3 returned Implemented with14 joined-owner files; sorted `path NUL content`
hash reconciled as `d2dc48eb39c733fd6a5fb0d718ff4059b91a0e9f0edaeecb8b39a9c27e8ea926`.
The original owned R3/TLS session and exact RawMessage/manifest interfaces are
available to T4/T5. Rust formatting and source parsing alone ran; native workflow,
build/profile/ShellCheck/CI observations remain Completion. The same Lead has
reliable foundation context and is reassigned serially to T4; T5 waits only
the shared fixture writer. No per-task review or passing receipt is required.

T4 source consumption found fixture-only composition deltas: producer mode
needs actual producer business state in its outbox transaction, and independent
store recovery needs publisher/consumer registrations in the existing example.
The released T2 example entry, example SQL and outbox adoption-doc owners are
assigned to T4 for that bounded closure. Effect logic and production runtime
role/concurrency APIs remain unchanged. This lock update closes existing B2;
it creates no new upstream decision or task-level proof boundary.

T4 returned Implemented after interruption recovery. Its explicit ordered
`path NUL content NUL`16-file digest reconciled as
`69522b353f2cb22faaf838bc7e87bcbdd0a2909a5e1ec075f2adeecfda47b6dc`.
Native restore holds the controller lock; all six scenarios account for actual
final stores and known identities. Static ELF/architecture/artifact identity
admission and one musl/static fixture build close runtime-image compatibility
across profiles. Only source parsing ran; actual build/profile/tests/review and
six native observations remain assembled Completion. All T4 writers stopped,
and the shared owner set is assigned to fresh T5.

T5 returned Implemented; its ordered `path NUL hex(file-SHA256) LF`11-file
digest reconciled as
`50eb44f1513e8b855fe8ae07f5ecb7005e4999c93072420faf9f9bddd26149a1`.
The accepted effect file remains
`49bd958de1a8b743e04481c52c77dcfe5d1106592c2e68a80bf4b78c8fc4dfb7`.
Bounded scheduled ingress, exact sized JSON bodies, ordinary probes, source
audit, telemetry, outage/catch-up and shared-admission evidence are authored;
no actual measurements, Cargo proof or acceptance are claimed. Missing required
observations are reported incomplete. All writers and descendants joined.

Completion now starts on one fixed assembled candidate. The delivery owner
selects one matching proof plan, reuses valid scoped T6 causal evidence, obtains
the real B2/B5 observations and three operating dispositions, and resolves one
fresh integrated review. Root retains Git/PR/CI publication custody; no merge
or deployment is included. The draft PR currently publishes preparation only.
