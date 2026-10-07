# T1 — Process-owned readiness progress failure

Outcome:
Replace indefinite alive-but-stale readiness-driver behavior in both roots with
the accepted terminal progress-loss path, preserving ordinary failed-round
recovery, generic unarmed health behavior and current cleanup/exit ownership.

Consumes:
- [R1](../spec.md#r1-required-progress-and-process-recovery) and
  [R3](../spec.md#r3-consumer-and-topology-truth) — process and topology behavior.
- [System R1](../design/system.md#r1-one-serialized-completion-contract) — closed state/arbitration mechanism.
- [Ownership H/S/W and Rust inverse map](../design/ownership.md) — exact owners.
- [Guidance closure](../design/system.md#guidance-and-delivery-closure) — #243 timing/topology disposition.

Provides:
- Health's narrow lifecycle capability wired into both real roots, proving code
  at health/root/process owners, and accurate runtime/persistence guidance.

Boundary:
H, S and W form one outcome: an unconsumed health API or a single armed root is
not its independently acceptable completion. Keep observer/report custody,
first-primary precedence, late-completion/stop arbitration, existing 18.5-second
tail and default bounds together. Include actual-root no-diagnostics topology
coverage; T2 owns database-backed consumer compositions. Preserve #254's
background/pool behavior. No new watchdog framework, public test seam, shipped
fault switch, schema, provider policy, transport wire change or runtime knob.

Mutable owners:
- `crates/health/src/lib.rs`, service bootstrap/shutdown and their owner-local
  tests; `crates/service/tests/lifecycle.rs` and `grpc_process.rs` only where
  root lifecycle/Watch-drain coverage needs the changed root boundary.
- Jobs-worker bootstrap/shutdown and owner-local tests;
  `test/tests/jobs/process.rs` for the existing worker process fixture.
- `docs/architecture/runtime-lifecycle.md`, `docs/architecture/persistence.md`:
  R1 distinction, pool/dependency interpretation, shared-scheduler/platform/no-
  diagnostics limitations, corrected 6/11/14 versus 16-second timing, current tail.

Exclusive locks:
- Health public lifecycle capability during H and root caller integration;
  both roots' shutdown ownership. These are T1-private across ledger tasks.
- Shared worktree target/CPU execution admission for any compile feedback;
  obtain it serially through the Orchestrator. It is not a mutation lock on T2 code.

Final validation:
- Claim: Both roots enforce mandatory armed progress loss independently of
  readers/diagnostics, preserve primary failure/stop/drain and bounded cleanup;
  generic and ordinary failed-round recovery remain valid.
- Checks: Matching build/relevant tests under current AGENTS/Build Speed plus
  accepted root/process proof boundary in System Design; exact cases and commands
  are Implementation-owned and execute at assembled Completion.
- Observable: State-local ordering and actual-root lifecycle evidence remain
  distinguished; expiry exits through existing failure semantics, without a
  scheduler-independent or fleet claim. Docs match current runtime authority.

Reopen if:
A required mechanism, root/profile lifetime or visibility cannot satisfy H/S/W
without changing accepted behavior; return placement to Rust Ownership,
mechanism to System Design, changed outcome to Specification. Routine test
construction and compile repairs remain here.

## Implementation handoff — 2026-10-06

```text
unit: T1
verdict: Implemented
candidate: base 699887b18594088a59bcc23a049d290d089f6da1; nine-path git diff --binary SHA-256 d5e5047021015f8517a1355d90d5157af8bdc1adedb095607271ec476c2f5db5
provides: unverified H/S/W code, owner-local and existing-process tests, runtime/persistence guidance
next_owner: Ledger Orchestrator for assembly; final Delivery Lead for consolidated validation/review
```

The bounded diff paths, in hash-input order, and current file SHA-256 identities:

| Path | SHA-256 |
| --- | --- |
| `crates/health/src/lib.rs` | `0b66f69efe44aad6526a1c14ea0148a7a7b5dbb78501b32d11c12deb5e4772ca` |
| `crates/service/src/bootstrap/mod.rs` | `4054b343682a6955a260481a24566e4b7ea763403dbc2bc2855d1d86673a4478` |
| `crates/service/src/bootstrap/shutdown.rs` | `44d1b73df7da5fca80d8dcab6484845aee8ee66339cf1e1d9a61da684afea177` |
| `crates/service/tests/lifecycle.rs` | `84b97f061a9c52a6058424100d237841a70cae7b3cf3f5501adf8d354189c63d` |
| `crates/jobs-worker/src/bootstrap.rs` | `451240cdb003752f6fbef057208b9b12b359e5bc35abb05ee423e0c81ce13d9c` |
| `crates/jobs-worker/src/shutdown.rs` | `ab3328760c20991ec3feebe296e3d83169c957cb1841f4d5ddaf44d793128136` |
| `test/tests/jobs/process.rs` | `a1cc8460403635ba4537a73bb979c05b851a18d5977a5cee2f168da884a88526` |
| `docs/architecture/runtime-lifecycle.md` | `b13dc0944069eaefa6cdf6ce822c9f34a7c7d385ab188ea116e474d91b4cda64` |
| `docs/architecture/persistence.md` | `9a064f8b516e8d2c138eebc5007c166f635ed693459e45e26fa025a511bebf42` |

Health now exposes narrow arm/inspect/stop/wait operations. Its existing watch
write boundary owns completion time, expiry and stop arbitration; terminal
evidence survives later completion and drain. Ordinary completed failed or
timed-out rounds renew progress. The single terminal log is emitted after the
lock and the existing ready gauge is withdrawn. No new dependency, feature,
configuration key or production test seam was introduced.

Service and worker arm after successful readiness admission and track both
driver and explicit-report observer before ready/work admission. The worker's
new private reporter writes its existing sticky watch. Both final stop paths
inspect retained loss before cancellation and preserve a prior primary error.
Existing shutdown plans, grace arithmetic and dependency closure remain owners.

Implementation-owned proving code (written, not executed):

- Health controlled-time cases cover admission age/equality, expiry without
  readers, late success preceding observation, stop/drain versus expired gaps,
  completed failures/timeouts and recovery, and one terminal log without a
  recovered event or ready gauge. Existing generic unarmed tests remain.
- Service bootstrap/shutdown and worker bootstrap tests cover root report
  custody, loss retained before observer reporting, timely stop and primary
  precedence. These are root-local state/lifecycle observations.
- `resumed_service_exits_one_after_progress_loss_without_diagnostics_or_probe_reads`
  and `resumed_worker_exits_one_after_progress_loss_without_diagnostics` use
  their existing real-process fixtures: confirmed SIGSTOP, a three-second gap
  against existing 500 ms interval/budget settings (`B = 2 s`), then SIGCONT.
  Their oracle requires terminal loss, primary failure/exit 1 and completed
  teardown. This observes resumed scheduling, not scheduler-independent restart.
- The existing service connection-cap case now includes diagnostics absent,
  distinguishes probe connection refusal from cached health, then releases
  capacity. Database-backed useful-work recovery belongs to T2.

Only scoped `rustfmt --edition 2024` ran over the seven changed Rust files.
No Cargo build/check, test, lint, docs check, per-task review or acceptance ran.
Before compilation, `df -h .` reported 702 MiB available on the data volume;
the Orchestrator withheld the shared compile slot. Static type diagnostics are
therefore unavailable, not passing. No cache cleanup, shared target, global
configuration change or new runtime environment was attempted. Final Delivery
must establish the required assembled build/test/runtime evidence; the same
owners remain available for resulting in-scope repairs.

The disjoint S/W execution lane
`/root/operational_continuation/t1_process_progress/root_integration` completed
and joined before this handoff. No T1 writer remains. T2 owns the agreed
metadata integration for three new outbox marker IDs in the existing worker
process fixture: `test-jobs-process-progress-nats-create`,
`test-jobs-process-progress-nats-argument`, and
`test-jobs-process-progress-nats-cleanup`. Their registration in the current
`scripts/lib/template_profiles.json` outbox list is outside this bounded diff;
the Orchestrator relayed it to T2. Canonical `tasks.md` was not edited here.
