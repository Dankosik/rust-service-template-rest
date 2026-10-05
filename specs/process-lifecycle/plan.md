# Process lifecycle implementation plan

Status: ready

## Fixed unit and authority

One fixed implementation unit: repair the ordinary service and jobs-worker
process lifecycle so stopped, failed, partially admitted and unwinding processes
retain their resources, stop admitting work, observe required task/listener
failures, and report bounded cleanup truthfully. The final delivery is one
separate PR. This is the single-unit path, not a task ledger.

Accepted inputs are [intent](intent.md), [specification](spec.md),
[technical design](design/overview.md), [source evidence](design/evidence.md),
and the [ready design transition](technical-design-transition.md) with its
[PASS review](technical-design-review.md). The design closes L1-L8 mechanisms,
APIs and placement; Implementation chooses coding details and tests.

Checkout: `/Users/daniil/.codex/worktrees/process-lifecycle/rust-service-template-rest`.
Branch: `codex/process-lifecycle-20261005`. Base:
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Design SHA256: `99ebb6925f9881f582a5bfb24ba3c7cefd45d373ed3a8801610b6b0f252db289`.
Evidence SHA256: `15d16825ca91b001bdcbae4ee82d403fe71f07384d9f3d5f2cafbb7603064d3d`.
Specification SHA256: `30b2f0124032cc290c670fbb37c983a7dc62d14c37defed0d44d193b46d9764f`.
These identities were read back at Planning entry; existing phase artifacts
are preserved. Repository workflow owners are from the same base.

The user has authorized genuinely necessary lifecycle repairs, local validation,
commit/push and one separate PR. The continuation root retains publication and
final delivery ownership. The Implementation Lead receives local code and
test-writing authority and returns `Implemented`; it receives no independent
publication, merge or deployment authority. Defaults, configuration values,
schemas, durable job/message behavior, infrastructure and profile choices stay
unchanged. Full deadline and validation repair is included. No new supervisor,
component registry or verification harness is authorized.

## Atomicity and dependency decision

The independently acceptable postcondition is the complete lifecycle contract
of both ordinary entry points and the native adapters they share. Splitting
adapter preparation from retained root ownership, failure observation from
terminal disposition, or deadlines from telemetry/runtime accounting would
produce layers that still require companion work to meet that postcondition.
The documentation describes this same result. There is no separate schema,
protocol, deployment or independently consumable outcome needing another unit.

All implementation prerequisites are available: ready behavior, mechanism,
exact owners, locked dependency APIs and writable checkout. No live provider or
test inventory is an implementation prerequisite. Establish the agreed adapter
APIs and dependency edges before consuming them in root edits; this is internal
coding order, not a partial acceptance gate. The Lead may use disjoint lanes
where useful, with serial integration and no overlapping writers. The root
does not need another App chat or worktree.

## Delta and writable owners

All rows belong to the single fixed unit. The design's
[placement map](design/overview.md#placement-and-dependency-decision) owns the
full API and failure semantics; the following map makes execution custody
explicit. No row is a separate task or a test execution assignment.

| Obligation | Current-to-target delta | Mutable source owners |
| --- | --- | --- |
| L1, L2, L6: service admission and unwind | Local/combined startup resources become immediately retained optional slots; stop and sticky failure race guarded admission/serving; all post-install failures and unwinds reach common cleanup and explicit runtime shutdown. Cache is retained before its probe. | `crates/service/src/bootstrap/mod.rs`, `shutdown.rs`, `cache.rs`, `postgres.rs` |
| L3, L4, L8: service-owned tasks | Existing JoinSet gains the design's private named Background owner and sticky failure observation; every process-owned spawn uses it; join results, forced abort acknowledgement and uncertainty remain distinct. | `crates/service/src/bootstrap/shutdown.rs`, `mod.rs`, `authn.rs`, `postgres.rs`, `idempotency.rs`, `webhooks.rs` |
| L1-L4, L6-L8: worker | Guard registration/admission, retain pool/provider/readiness/messaging/listeners and each started engine immediately, observe faults before claiming, keep TaskTracker and native engine/consumer completion owners, retain registered-task abort handles, replace `abort_startup` with common cleanup. Replace detached signal pump/watch counter with directly owned native receivers and explicit closure failure. | `crates/jobs-worker/src/bootstrap.rs`, `shutdown.rs`, `lib.rs` |
| L1, L2: PostgreSQL admission | `prepare_pool` constructs the native lazy pool, `admit_pool` verifies on one acquisition within 11 s, and `connect` delegates with existing 3 s failed-close allowance. Roots retain the pool before awaiting; preserve session/query and native return semantics. | `crates/infra-postgres/src/pool.rs`, `lib.rs` |
| L1, L2: broker admission | Concrete `MessagingStartup` retains connected client plus Closed receiver before topology awaits; `Messaging::prepare`, holder `admit`/`close`, and delegating `connect` preserve existing 5 s admission and native drain semantics. | `crates/infra-messaging/src/messaging.rs`, `lib.rs` |
| L2-L4, L7: listeners | Retain independent `Server::failure` observation, force connection cancellation and one drain deadline from before the first await. Keep `Drained::TimedOut` connection-only after successful accept join; add voting `ServerError::AcceptTimeout` and retain accept failures while cleaning connections. Drop requests cleanup without claiming completion. Roots install named watchers immediately after retention. | `crates/infra-http/src/server.rs`, `lib.rs`, both roots listed above |
| L2, L5, L6: provider | Export existing SDK slack and implement `shutdown_until`; existing `shutdown` delegates. Explicitly attempt flush for every installed provider, including zero remaining budget; distinguish observed completion from an incomplete blocking wait. | `crates/infra-telemetry/src/traces.rs`, `lib.rs`, both roots listed above |
| L4, L5, L7: whole process accounting | First stop/failure retains D; async stages end by D minus existing 1 s runtime reserve. Use existing stage ceilings, share dependency-stage time with forced acknowledgements, include SDK 500 ms slack, and validate `grace >= drain + 18.5 s` including equality. Preserve failure precedence 1, clean stop 0, degraded stop 3 and repeated-signal expedite behavior. | Both roots' `shutdown.rs` validators/Budget/stages and runtime boundaries in service `bootstrap/mod.rs` and worker `lib.rs` |
| L6 and retained profiles: unwind dependency | Add service's unconditional normal futures-util edge and reconcile gRPC-only dev entry; move worker edge outside jobs removal marker, preserving features. Reuse locked 0.3.34 with std; no upgrade. | `crates/service/Cargo.toml`, `crates/jobs-worker/Cargo.toml`; Cargo-generated `Cargo.lock` dependency edges |
| L1-L8: usable contract and adequate coverage | Reconcile lifecycle/integration, finite pool admission, whole-tail arithmetic, outcomes, blocking limits and worker registration rustdoc; write or adapt meaningful coverage with the production edits. | `docs/architecture/runtime-lifecycle.md`, `docs/configuration-source-policy.md`, `docs/architecture/persistence.md`, worker `lib.rs`; directly contradictory retained-profile guide statements; existing affected crates' adjacent tests and `tests/` files |

The Lead owns these production, documentation and corresponding existing test
surfaces exclusively for the unit. It may add an ordinary test file within an
affected crate when necessary; it must not create a new test harness, stack or
runner. Unit execution state and chosen final commands belong in
`specs/process-lifecycle/implementation.md`, created by Implementation when it
first has execution state. Preserve the accepted intent/spec/design and the
Planning review receipts; an invalid accepted input returns to its named owner.
Mechanical caller or test updates for these APIs remain in the same unit.
Unexpected overlapping writers require serial reconciliation, not a new unit.

## Canonical sources and compatibility

Handwritten adapters and composition code are authoritative. Cargo generates
only the intentional lock edges after manifest edits; validation uses the
result with `--locked`. Existing `connect` and telemetry `shutdown` APIs remain
delegating compatibility entry points with actual callers. Remove superseded
worker startup cleanup and detached signal forwarding; do not retain parallel
lifecycle policy. No new crate, module or dependency version is selected.

Keep PostgreSQL, auth, cache, object storage, idempotency, webhooks, gRPC, jobs,
messaging and outbox source-removal markers coherent across fields, imports,
spawns and cleanup. Service Background and both applicable unwind edges are
unconditional; messaging-only worker output must retain unwind support.
Missing resources skip their own work and still use the common exit path.
Keep `Registration::spawn`/`shutdown`, `service::run`, operator CLI policy,
request/wire contracts and durable settlement/replay unchanged. Native library
tasks continue to use native owners. OpenAPI/protobuf/schema generated bytes
have no selected change; do not edit them as a side effect of lifecycle repair.
Normal replacement uses one source revision, with no migration or mixed-version
protocol stage. Deployment is outside this deliverable.

## Final observable success and proof ownership

Every accepted L1-L8 obligation above must be implemented. In particular,
stopping a worker during stalled admission ends admission without claiming,
cleans retained resources and attempts provider shutdown under the original
deadline; a cleanup panic degrades a stop, while an already selected admission
failure retains exit 1. A later service bind failure cleans an earlier bound
listener with live connections and its provider, then explicitly shuts down the
runtime with exit 1. Required task or accept-loop failure is observed before
ready/claiming or while serving. Cooperative completion, forced completion,
failed joins and unconfirmed work have truthful outcomes; only confirmed
accept completion followed by diagnostics connection timeout keeps the scrape
exception. Runtime return never certifies terminated blocking work.

The corrected minimum is 43.5 s for default 25 s drain, inside unchanged 45 s
grace. Equality is accepted and lower totals refused before runtime creation.
The component integration guidance matches the actual private service wiring
and public worker registration path.

Implementation owns concrete tests, fixtures, assertions and exact final
commands. There is no test-design phase or missing test-plan prerequisite.
After the whole unit is implemented and all writers have joined, the assigned
delivery owner performs one assembled validation boundary under
[Implementation](../../docs/spec-first-workflow/phases/implementation.md),
[Validation budget](../../AGENTS.md#validation-budget),
[Validation Routing](../../docs/validation-routing.md) and the
[Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md).
The existing broad-Rust criterion applies to the manifest/multiple-crate
change; documentation consistency and applicable dependency policy remain
with their existing owners. No full-repository gate is added by this plan.
The final independent Implementation Review covers the assembled lifecycle
change because its behavior materially affects concurrency safety and truthful
resource completion. Resolve blocking findings before local acceptance.

Known CI-owned categories are existing source initializer/profile generation,
real PostgreSQL proof, messaging integration, and selected runtime-image,
image-lifecycle/migration/security gates. The final changed-path classifier and
CI configuration decide which apply; this list does not create a second matrix
or mandatory local infrastructure. Preserve existing CI gates. The delivery
root publishes the reviewed branch as one PR and obtains the selected CI
results there. Local acceptance and the PR/CI outcome remain distinct.

Report observed database behavior only with the real-database proof required
by the persistence validation owner. Source-only NATS/SQLx/OTLP reasoning is
not live provider evidence; the prior macOS run of Linux-only gRPC process
tests executed zero cases. Neither that run nor a skipped case is a pass.
Unavailable optional local provider/image environments do not block coding or
ordinary local acceptance, and create no provisioning obligation. Missing
required build/test evidence or a selected external gate stays explicitly
incomplete for its actual scope.

## Readiness walkthrough and handoff

A fresh Lead can start from this file and the ready design: the root slots,
adapter APIs, failure precedence, numeric budget and profile obligations are
closed. It can update adapters, wire retained ownership and observations in
both roots, replace old cleanup paths, reconcile manifests and documentation,
and write tests within the same unit. There is no unavailable input at the
first implementation action and no intermediate state being offered as an
independently acceptable deliverable. Native API incompatibility reopens
System Design; an unavoidable observable contract change reopens Specification;
requester meaning or authority change reopens Intake. Routine coding/test
choices and mechanical locator/lock updates stay with Implementation.

Implementation carrier: the existing root dispatches a fresh native
general-purpose Acceptance-Unit Lead on this checkout, using
[Implementation](../../docs/spec-first-workflow/phases/implementation.md) as
Method and this fixed unit as its boundary under the
[Codex harness](../../docs/agent-harness/codex.md). The Lead returns
[Acceptance Result V1](../../docs/spec-first-workflow/interfaces/acceptance-result-v1.md)
`Implemented` with the bounded diff and joined writers. The root then assigns
the single final validation/review boundary and retains PR publication. No
separate App chat, synthetic ledger or per-row acceptance is needed.
