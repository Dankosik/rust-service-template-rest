# T2 — Useful-work recovery through real consumers

Outcome:
Close the missing consumer evidence for existing recoverable dependency/capacity
behavior: separate local instance state, real HTTP/gRPC useful work and live
health Watch recover at the accepted two-instance boundary.

Consumes:
- [R2](../spec.md#r2-recoverable-pressure-and-dependency-failures) and
  [R3](../spec.md#r3-consumer-and-topology-truth) — required recovery distinction.
- [System consumer composition](../design/system.md#r2-and-r3-recovery-through-actual-consumers)
  and [Ownership C/P/G](../design/ownership.md) — actual integration and profile owners.
- Existing `test/tests/postgres.rs` pool/relay/runner and retained transport
  contracts. T1's new arming API is not needed for this existing-behavior fixture;
  actual-root lifecycle/topology proving code stays with T1.

Provides:
- Existing-runner dependency-backed consumer and isolation proving code,
  profile-correct imports/dev edges and documentation of the finite claim.

Boundary:
Keep the instance compositions, real dependency work, TCP consumers and fixture
cleanup one independently consumable proof outcome. Each instance owns its own
pool/health/refresher/listeners; shared PostgreSQL is intentional. No shipped
business route, new binary/framework/runner, production tuning, uncertain-effect
replay or claim of independent OS schedulers/fleet performance. Preserve HTTP
coverage when gRPC is removed and retained auth/profile conjunctions.

Mutable owners:
- `test/tests/postgres.rs` and optional cohesive
  `test/tests/postgres/operational_recovery.rs`; `test/Cargo.toml` only current
  resolved workspace dev edges. If dependency graph edits mechanically require
  `Cargo.lock`, this Lead owns that explicit scoped update, never a validation
  side effect or version upgrade.
- `crates/infra-grpc/tests/transport.rs` only an uncovered pure propagation
  claim; root process tests remain T1-owned.
- Existing profile/marker dispatch in `scripts/lib/template_profiles.json`,
  the responsible `scripts/lib/template_*.py` and `template-owned.paths` only
  when its path family needs change for C; canonical sources before projections.
- `docs/grpc.md`, `docs/validation/postgres.md` for consumer/useful-work scope.

Exclusive locks:
- Existing profile/projection metadata bundle, shared serially with T3/T4;
  no second marker implementation or concurrent edits to that semantic owner.
- Shared worktree target/CPU admission for any bounded compile feedback.

Final validation:
- Claim: Real dependency-backed HTTP/gRPC work succeeds before and newly after
  ordinary recovery; live Watch propagates loss/recovery; A-local pressure does
  not alter B through template state; correlated failure remains recoverable;
  absent-diagnostics connection pressure releases to useful work at fixed capacity.
- Checks: Accepted real PostgreSQL integration boundary and current retained-
  profile gates, collected once at assembled Completion through the existing
  runner/classifier. Heavy checks remain CI-owned as routed. Implementation
  chooses fixture controls, assertions and exact commands alongside this code.
- Observable: Loss/recovery and newly completed useful work per instance,
  bounded failure class/completion evidence and joined fixture resources;
  real-root exit evidence remains T1. A green direct reader alone is insufficient.

Reopen if:
The retained adapter/profile composition requires a production seam or changed
policy, or current evidence shows cross-instance hidden shared state. Route the
smallest mechanism/ownership issue upstream; missing test recipes remain here.


## Implementation handoff

The existing `postgres` integration target includes
`postgres/operational_recovery.rs`. Its single cohesive scenario,
`consumers_recover_from_local_pressure_shared_interruption_and_admission`, owns
two separate admitted pools (one slot each), readiness owners at the existing
2 s / 4 s / three-failure policy, refresh tasks and TCP listeners. Each HTTP
operation and retained gRPC Echo reads a value independently updated through
the SQLx fixture's direct database connection. Both standard Check and one live
Watch per instance observe the same canonical reader. All listeners, streams,
refreshers, pools and relay tasks have bounded successful-path teardown.

The scenario holds A's slot while proving the connection still answers SQL,
observes A's acquisition failure and B's new useful work, and then releases A.
A shared reversible byte relay next interrupts both dependency paths; it rejects
new connections and ends existing relays until release. This private relay is
necessary because the existing commit-fault relay deliberately owns one-shot
uncertain-effect faults, not a recoverable all-connection interruption. No
protocol, retry, durable-effect, or production policy is added. Completed
PostgreSQL failures remain non-stale across an interval longer than freshness;
release must deliver newly written data through both transports. Finally four
admitted application sockets block new liveness/readiness/useful requests with
diagnostics absent; release restores the same listener and unchanged pool.
The actual-root/process and independent scheduling boundaries remain T1's.

The profile boundary is PostgreSQL-retained HTTP for every existing auth mode.
DB-backed Echo/Check/Watch use the existing transport fixture conjunction:
PostgreSQL + gRPC + auth none/introspection. Real TLS introspection uses the
existing fixture support; the production gRPC authentication chain is retained.
JWT keeps HTTP coverage and its existing transport/process owner, with no new
DB-backed gRPC JWT claim. The existing non-nested marker dispatcher projects
these conjunctions; there is no new marker grammar or proof runner.

Implementation feedback: scoped `rustfmt --edition 2024` formatted and parsed
`test/tests/postgres/operational_recovery.rs`.
No Cargo check, runtime scenario, lint, aggregate validation, or review ran in
this lane. The coordinator's serial resource preflight reported only 702 MiB
free, so compile feedback was not admitted. This is an unavailable local type
check, not a successful compilation. Required proof remains with assembled
Completion and may use the current actual-head CI route.

The existing database runner can select the scenario with:

```bash
bash scripts/ci/test-integration-db.sh --test postgres consumers_recover_from_local_pressure_shared_interruption_and_admission -- --nocapture
```
The full required PostgreSQL and retained-profile gates remain the existing
runner/classifier's responsibility, collected once for the assembled candidate.


The canonical lockfile transformation helpers in `scripts/lib/template_init.py`
added exactly four missing `integration-tests` direct dependency edges from the
manifest: `grpc-contracts`, `infra-grpc`, `tonic`, and `tonic-health`. The
transformation retained every existing package identity, version, checksum and
other package block. The coordinator then admitted one bounded
`cargo metadata --locked --offline --format-version 1`: exit 0, 75 manifest
dependency declarations for `integration-tests`; no fetch, compile, build script,
or application/test execution. This closes the source graph only.

Joined writer: `t2_consumer_recovery/profiles_docs` implemented the disjoint
metadata/manifest/docs scope and returned its final result. Its initial static
JSON parse and whitespace check passed; neither is runtime or task acceptance.
The profile owner also integrated the coordinator-requested three T1 outbox
marker registrations (`test-jobs-process-progress-nats-create`,
`test-jobs-process-progress-nats-argument`,
`test-jobs-process-progress-nats-cleanup`) without editing T1's source. All T2
writers are joined; the shared profile bundle is available to T3.
