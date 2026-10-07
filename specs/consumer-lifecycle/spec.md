# Consumer lifecycle

Status: ready

Requester meaning: [Intent](intent.md). Evidence and feasibility:
[Research](research/synthesis.md). Definition covers behavior and proof
meaning; Technical Design chooses mechanisms and placement, and Implementation
chooses concrete cases and commands.

## Outcome and scope

An initialized service can complete its first release, accept a later template
runtime update without losing service work, and rehearse compatible rollout
and recovery. The native image gate becomes measurably faster while observing
the same artifacts and required invariants.

The four outcomes are cumulative. A guide, an unexecuted script, successful
source tests, or a locally built image cannot alone satisfy the requested
release/recovery/CI observations. Missing external authority leaves those
outcomes outstanding and allows independent local work to continue.

| Accepted item | Behavioral disposition | Necessity |
| --- | --- | --- |
| First real consumer release cycle | R1–R3: actual derived repository and verified registry-to-runtime cycle | User-accepted improvement 1 |
| Safe runtime upgrades | U1–U4: reviewable, provenance-backed change preserving service policy/code | User-accepted improvement 2 |
| Mixed-version and recovery proof | D1–D4: practical durable-state admission and recovery with explicit limits | User-accepted improvement 3 |
| Faster native image CI | C1–C3: comparable measurement and a shorter critical path without weakened proof | User-accepted improvement 4 |
| Existing source/generated admission and four image shapes in PR250 | Consume their admitted result and identities; do not reimplement or remove them | Existing accepted dependency |
| Portable sync and one-time profile selection | Deliberately unchanged | Existing ownership and initializer contracts |
| Production SLOs, topology and data-loss policy | Remain service-owned; rehearsal measurements do not populate them by default | Existing Production Contract |
| Template-wide release announcement, topics, badges and listing | Remain with the original Stage 12 owner; this task cannot mark the entire stage done unless its remaining criteria are also fulfilled | Existing roadmap boundary |

## R1. A real derived repository

The rehearsal uses a service with its own repository identity, initialized
source, committed lockfile, service name and revision history. It keeps the
chosen profile tuple and generated contracts traceable to the template
revision. An in-place template build or transient uncommitted initializer
directory is not the requested real-consumer result.

Exercise a minimal initialized output and one PostgreSQL-bearing output as
required by Stage 12. These are separate initializations; changing the profile
of an already initialized service is not an upgrade route. One named consumer
must traverse the entire published release cycle. The durable rehearsal may
retain PostgreSQL, jobs, JetStream and outbox in one already-supported tuple
so their interaction is observed without a Cartesian profile matrix.

Use synthetic data and no production traffic under assumption A1. Technical
Design chooses the smallest representative setup and whether the minimal
output can reuse applicable initializer proof. It must still obtain the
explicitly requested real-consumer result rather than merely reference the
template matrix.

## R2. Published artifact identity and trust

The release record distinguishes the upstream template revision, initialized
consumer revision, upgraded consumer revision where applicable, exact CI
candidate, local image ID, published image digest, publication workflow/ref,
and running digest. An image ID is not an OCI registry digest. When a CI run
uses a merge revision or a private tracked-tree candidate, retain it alongside
the PR head rather than silently relabeling it.

The consumer follows the existing publication sequence: admitted exact source,
build, hardened lifecycle, vulnerability policy, SBOM, push, sign and attest,
registry readback verification, then tag promotion. Verify the expected
repository/workflow/ref and issuer, plus provenance and SBOM subject against
the digest. The service release tag matches the service Cargo version.

A missing, failed or cancelled selected gate, wrong subject, wrong signing
identity, missing expected attestation, or failed registry readback prevents a
verified-release claim and tag promotion. A run-scoped candidate left by a
failed publication is not a released image. Repeated attempts reconcile
existing run/tag/digest state before proceeding; a tag is never treated as
immutable identity.

Keep credential values out of the evidence. Required repository/plan support
for the existing attestation mechanism is checked before publication; changing
repository visibility or weakening verification is not a recovery fallback.

## R3. Run and rollback mean observed operations

Run the verified image by its registry digest in the chosen non-production
runtime and observe its expected commit, readiness, selected dependency
admission, and shutdown under the current lifecycle contract. Keep the runtime
observation distinct from source or generated-output checks.

Retain two distinguishable, previously verified consumer image digests with
their admitted compatibility relationship. After the newer one runs, restore
the prior digest and observe that prior revision serving/working under the
declared durable-state rules. Rebuilding old source or running the same digest
twice is not rollback evidence. A first release may establish the first digest;
the later compatible consumer change establishes the second.

An image rollback does not undo database migrations, external effects or broker
state. Before rollback, compatibility admission must say that the prior binary
can use the resulting schema, queue and wire/state versions. If it cannot,
stop before replacing a working release and follow the declared recovery
procedure; never claim clean rollback from a process merely starting. Record
partial failures and the currently running digest so recovery is resumable.

## U1. Upgrade inputs and support boundary

An upgrade starts from the known template baseline that produced the consumer,
the exact later template revision, the consumer's current committed state,
and the retained initialization choices. These inputs distinguish a template
change from service changes in shared files. The service identity and selected
profiles remain stable; a profile change continues to require its separate
accepted migration work.

Generation uses explicitly selected trusted source revisions and their matching
generation rules. A provenance string alone is not authority to execute a
new external repository's scripts or hooks. Missing trust/admission is diagnosed
before execution, without borrowing publication credentials for preparation.

Support both new consumers with complete baseline evidence and existing
initialized consumers whose evidence can be recovered unambiguously. A legacy
consumer must first obtain an explicit, reviewable baseline adoption result;
the operation cannot guess missing settings or declare its current mixed tree
to be pristine generated output. An unavailable old revision, ambiguous
provenance or contradictory profile/identity inputs yields a diagnostic with
the missing input and no mutation of the consumer's accepted state. Arbitrary
repositories without a recoverable baseline are outside automatic upgrade
support, but the limitation is reported before effects.

## U2. Reviewable consumer-preserving result

The maintainer sees the complete proposed additions, modifications, deletions
and unresolved conflicts before accepting an upgrade. Business crates,
business migrations, local dependencies, routes, configuration choices,
deployment policy, identity and service-specific operational documents remain
owned by the consumer. A template change to a shared file may be proposed, but
never silently wins over a conflicting consumer change. Consumer-only paths
cannot disappear because the newer template lacks them. Renames and upstream
deletions that collide with local work require a visible disposition.

Preparation preserves the consumer's branch/index, tracked changes, untracked
and ignored files. Applying an upgrade requires a clean target or an isolated
candidate whose adoption cannot overwrite that work; dirty-state handling is
explicit and never automatically stashes, resets or discards it. Conflicted or
interrupted preparation cannot advance the accepted upstream baseline. Abort
leaves the original consumer recoverable with its pre-upgrade state intact.

## U3. Acceptance, repeatability and generated authority

A prepared diff is not an accepted upgrade. Only the reviewed, resolved and
validated consumer candidate advances its recorded template baseline. Every
changed dependency resolution is visible in the lockfile; validation uses
locked Cargo commands. Generated contracts are regenerated from their source
owners and checked, not chosen as independent merge truth.

Applying an already accepted target revision produces no additional change or
duplicate migration. A new attempt after failure explains whether no candidate,
a prepared/conflicted candidate, or an accepted upgrade exists. It cannot
report success from partial writes or stale receipts. The next upgrade uses
the last accepted baseline, preserving all intervening business work.

Validation is selected from the changed consumer surfaces and each repository's
owner. A clean textual merge alone does not claim semantic compatibility;
runtime and deployment compatibility have the separate R3/D1–D4 obligations.

## U4. Existing update ownership

Portable instruction sync retains its current manifest boundary and refusal
rules. It does not acquire Cargo, runtime, Docker or service-policy ownership.
The runtime update path may use existing generation and Git facilities but
must keep the two scopes and baseline meanings explicit. No new backup format,
generic package updater, hosted upgrade service or runtime dependency is
required by this contract; any added mechanism must address a demonstrated gap
in Technical Design.

## D1. Mixed-version admission

For each retained PostgreSQL/jobs/JetStream surface, the rehearsal identifies
old and new binaries, schema/migration history and message/job contracts, plus
the allowed overlap or required stop/drain transition. It observes actual
admission and a representative durable operation across the admitted
transition. It also observes the nearest incompatible boundary refusing before
unsafe work, at the existing runtime guard or the operator's explicit launch/
rollback admission procedure as appropriate. This does not require adding a
fleet-wide runtime detector for an operator-owned custody transition.
A schema-history admission check alone cannot prove old business
queries remain compatible; evidence and claims stay with the retained
template-owned mechanisms and representative consumer behavior exercised.

PostgreSQL follows the existing forward/expand-compatible migration contract
and its accepted matching prefix. Jobs obey the existing upgrade and custody
gate: apply the additive migrations before corrected workers start, and stop
or replace every old retention owner before relying on retained-failure custody
or activating recovery. Applying the additive migrations while old workers
still run remains admitted; their presence does not establish custody, and an
old-binary rollback restores the old retention behavior.
JetStream retains its logical event identity, explicit schema
versions and settlement rules. No blanket rolling-upgrade or binary-downgrade
promise is introduced. Technical Design pins the exact pair and explains its
allowed transition from those owners.

## D2. Recovery preserves durable truth

Use the existing harnesses and provider-native backup/restore facilities.
Restore only synthetic rehearsal state into an isolated destination. Preserve
the original snapshot and pre-recovery evidence until the result is accepted.
An incomplete, corrupt, incompatible, or internally inconsistent backup is a
failed recovery attempt, not an empty successful service.

The recovery boundary includes schema/history, application data used by the
rehearsal, jobs queue/history/identity sequence, and the applicable broker
stream configuration, messages and durable-consumer state. Queue identity and
sequence restoration must follow the current jobs owner. Backup success alone
does not establish restore success; inspect the restored identities and execute
the retained mechanism after restore.

Independent PostgreSQL and JetStream snapshots are not one distributed atomic
snapshot. Stop/fence writers and claimants where the selected recovery sequence
requires it. Reconcile publications, receipts and durable business-effect
identities across the restored stores before admitting normal consumption.
Ambiguous publication or ACK loss may cause replay; a successful restore must
not authorize duplicate durable effects or manufacture certainty that the
stores cannot establish. Broker deduplication windows and delivery-attempt
counts do not replace durable logical-ID effect reconciliation.

## D3. Bounded proof and production policy

Report measured recovery elapsed time, observed recoverable data boundary,
replay/duplicate handling and any unresolved effects for the exact synthetic
environment. These measurements are neither an accepted production RTO/RPO nor
a guarantee for a different PostgreSQL version, backup kind, broker version,
replication topology, volume size or live write load.

The service must accept its RPO, RTO, backup custody/retention, recovery owner,
mixed-version window and effect-reconciliation policy before production use,
as the existing Production Contract requires. If those inputs are absent,
production promotion/recovery remains unavailable; the local template proof
and reusable operator procedure can still be completed. Never fill business
loss tolerances from successful local measurements.

## D4. Scope of observed mechanisms

Cover PostgreSQL restore plus migration admission, jobs restart/upgrade and
queue/history/sequence recovery, and JetStream snapshot/restore plus replay
settlement in the smallest representative retained compositions. Reuse existing
coverage for unchanged details. A local single-node broker result cannot claim
cluster/failure-zone durability, nor can an adapter roundtrip claim a restored
consumer. Every observed result names the actual provider version and native
tool. A discovered provider limitation bounds the supported rehearsal; do not
silently upgrade providers or ignore it to make proof green.

## C1. Comparable CI measurement

The observed reference is PR250 CI run `37381047660` at PR head
`c3a3f18752ec8feee03a56a00b0edea2240d3b44`: image job `112002891386`
took 2,478 seconds, including 1,828 seconds in the derived-artifact receipt.
That cache-enabled observation is not a cold-build guarantee or a user deadline.

Measure source image work and selected initialized image work separately,
including generation, dependency/cache work, linking, lifecycle, scan, SBOM,
queue/wait time and the whole selected-gate critical path where observable.
Compare the same artifact selection, runner class/architecture, toolchain,
locked graph, proof policy and declared cache condition; report differences
that prevent attribution. Also report total runner work and cache/storage
effects so parallelism does not hide a cost increase.

## C2. Faster delivery retains artifact proof

Reduce the critical path through reuse, ordering, or bounded scheduling supported
by measurement. A claim of improvement requires comparable native evidence;
estimates or a faster local build alone do not satisfy this outcome. There is
no invented percentage or minute target. If comparable evidence cannot show a
reduction, report that outcome as incomplete or reopen the technical strategy.

Each selected release shape retains its exact initialized source/lockfile,
expected binaries, immutable image identity, hardened lifecycle, vulnerability
policy, SBOM and applicable runtime-inventory/admission proof. Aggregation must
fail when any selected required result is missing, failed, cancelled or belongs
to another candidate/shape. Concurrency, cache reuse and artifact transfer must
not turn one shape's successful proof into another's result. Keep publication
verification tied to the digest that is actually promoted and run.

## C3. Factored scope and existing defaults

Keep the existing separation between profile projection checks, distinct runtime
graphs, representative release shapes, integration infrastructure and agent
harness carriers. Runtime images do not multiply by harness choice. Heavy
database/broker proof follows the relevant changed mechanism, not every
projection. Preserve PR250's four representative shapes `1,7,47,65` and its
selection semantics unless evidence and the owning design explicitly establish
equivalent coverage; faster CI cannot be achieved by dropping a required shape.

Do not change the release optimization profile, runtime defaults, security gates
or tool pins merely to lower a timing. Any such proposal needs a current
accepted behavior reason and its ordinary owner. Cache absence remains a
correctness-safe rebuild condition; only trusted events write shared caches.

## External boundary and completion

Definition and local preparation do not authorize remote repository creation,
visibility changes, settings, pushes/tags/releases, GHCR writes, hosted runtime
creation or production-data access. Before the first such action, the
coordinator must have a concrete candidate with proposed owner/repository and
visibility, registry names, release refs, attestation support, runtime target,
synthetic data scope, permissions, cost/quota ceiling, retention and recovery
consequences. Seek only the missing user-owned effect authority then.

All already-authorized technical work continues up to that boundary. Publish a
completion record that distinguishes local support, exact CI observations,
registry verification, run/rollback, and durable recovery. The overall accepted
outcome remains open while an explicitly requested external or empirical result
is still missing. Source readiness is not Stage 12 completion.

## Representative composition and feasible falsifiers

A maintainer initializes a named consumer from template T0, adds a small
service-owned behavior and local configuration, accepts a reviewed T1 runtime
upgrade, publishes versions A and B, runs B by its verified digest, performs the
declared synthetic-state recovery, and returns to compatible digest A. The
business behavior/local settings survive, generated authority stays coherent,
the durable operation converges under replay, and every receipt names its own
source and artifact. The factored CI route proves the same selected shapes
with a shorter comparable critical path. This scenario composes the accepted
outcomes; it does not require a new permanent demo application or runner.

The nearest falsifiers are loss of a consumer edit, acceptance without a known
baseline, a conflicting update silently accepted, wrong/missing trust evidence
accepted, rollback starting an incompatible old process, restore losing durable
identity or duplicating a reconciled effect, and a faster CI aggregate accepting
an unobserved shape. Existing harnesses and native tools can expose these
boundaries; the executor chooses concrete cases during Implementation.

## Next owner and reopen conditions

Technical Design must close baseline reconstruction and upgrade custody,
consumer/release topology, version-pair compatibility and native recovery
sequence, release evidence custody, and the measured CI strategy. It must
resolve PR250 integration/admission from the current accepted revision rather
than duplicate open work. Its output supplies one dependency-ordered path for
Planning and concrete external parameters for the coordinator.

Reopen Definition only for a changed desired service/production scope, a needed
profile transition, changed proof/security requirements, or evidence that the
accepted behavior cannot be achieved within this boundary. Missing technical
inputs return to Research/Technical Design; missing external authority returns
to the coordinator without discarding independent ready work.
