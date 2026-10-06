# Consumer release and durable recovery

Status: ready. Implements [R1–R3 and D1–D4](../spec.md) under
[System Design](system.md). This is the operational design, not an execution
receipt or production recovery policy.

## Local consumer and version preparation

Prepare two full initialized Git repositories from trusted local template
objects, with native initialization commits, their own branch history and
committed lockfiles. They are disposable synthetic consumers, not additional
template worktrees or edits to a production service.

| Consumer | Exact initialization choices | Use |
| --- | --- | --- |
| `lifecycle-minimal` | service name `lifecycle-minimal`, repository `https://github.com/Dankosik/rust-consumer-lifecycle-minimal`, description `Synthetic minimal consumer rehearsal.`, codeowner `@Dankosik`; database/authn/outbound-http/outbound-auth/grpc/http-idempotency/jobs/messaging/outbox/webhooks/inbound-webhooks/cache/object-storage all `none`, harness `core` | Real local clone, public initialization, committed generated contract/lock and build. No remote repository or publication needed. |
| `lifecycle-demo` | service name `lifecycle-demo`, repository `https://github.com/Dankosik/rust-consumer-lifecycle-demo`, description `Synthetic consumer lifecycle rehearsal.`, codeowner `@Dankosik`; `database=postgres`, `jobs=postgres`, `messaging=nats-jetstream`, `outbox=postgres`, all other capability selectors `none`, harness `core` | Full release/upgrade/rollback cycle and representative durable composition. The tuple already exists; no profile or harness matrix is added. |

The proposed GitHub identities are local preparation inputs, not authority to
create those repositories. If the eventual accepted owner/name differs,
regenerate the pristine consumer before business edits or publication; do not
use runtime upgrade to migrate initialization identity.

For the durable consumer, full render at upstream
`2cb871895b9edd018205fc98223477e269fce2e9` supplies release A's baseline. Use
the completed updater from admitted revision F as the orchestration tool while
executing the **2cb source's own** initializer. Capture/adopt A's baseline,
retain a small committed consumer-owned change, prepare and accept an upgrade
to F, and change the consumer Cargo version to `0.1.1` for B (`0.1.0` for A).
No template binary version is changed merely to create these consumer releases.
Record version changes and their lockfile effects as consumer work.

Freeze A/B source commits before their native CI. Both contain corrected jobs
custody, the same compatible migration set and retained kinds/event versions.
The new updater must preserve the consumer-owned change. F's unexpected
runtime or migration delta reopens compatibility before publication. A/B
digests are produced by the existing publication path and must differ.

The negative historical transition separately uses
`67be869acea112af271ec8ba621cbc50ae9d36b7` (pre-custody) and `2cb8718…`
(corrected), each with its own full initialization helper, toolchain and
lockfile. Their SQL migration delta adds only
`20261002150001_add_background_job_recovery_history.sql` and
`20261002150002_index_failed_background_jobs.sql`; existing migration bytes
are unchanged. Retain two actual compiled adapter/worker fixture processes,
with a common reviewed synthetic fixture overlay applied **after** pristine
render. Record upstream revision, pristine render, overlay hash and executable
hash separately. The overlay cannot patch production adapter/migration/runtime
source or silently upgrade a dependency. It provides the same stable job
kind/payload and typed event v1 to both builds through existing APIs.

For this historical pair, add **no consumer migration to the embedded set**.
Synthetic effect/inspection tables are created by the existing test fixture
outside migration bookkeeping. Thus both new framework versions are above the
old embedded maximum, and old-history admission can honestly observe the
matching-prefix rule. A real consumer whose own later migration raises that
maximum may instead reject those previously unknown in-range additions even
when their SQL is additive. That case is not a rolling-overlap promise; the
upgrade migration gate in [the runtime mechanism](runtime-upgrades.md#prepare-resolve-validate-accept)
keeps it unaccepted until the service owner supplies a forward disposition.
Never renumber released/applied migrations or infer compatibility from DDL alone.

## Smallest deployment graph

| Node/edge | Owner and contract | Target and mixed-version rule |
| --- | --- | --- |
| Consumer Git → native CI → CD → GHCR | Consumer source, existing `ci.yml`, `codeql.yml`, `cd.yml` and composite publish action | Proposed named repository and one image repository. Exact candidate admission; no cross-repository workflow reuse that changes signing identity. |
| Published digest → `/service`, `/migrate`, `/jobs-worker` | Existing hardened image/runtime inventory and lifecycle | Existing local Docker, `linux/amd64` with explicit platform/emulation on an ARM host if supported. Failure of that runtime capability is a concrete input gap; do not silently rebuild an unverified host-native substitute. |
| Service/worker → PostgreSQL | Existing pool/session/history admission and migration owner | Isolated Compose network, pinned database image, direct migration connection. Observe service readiness with the database enabled; workers use the admitted connection allocation. |
| Worker publisher/consumer → JetStream source and DLQ | Existing outbox, message identity/schema and settlement contracts | Single-node pinned NATS 2.15.0, file-backed named streams and durable. Stable v1 handlers are available in each overlapping fixture. Unknown versions retain existing DLQ behavior. |
| Synthetic business state + jobs/outbox + messaging effect | Existing integration fixture owns finite data; PostgreSQL is the effect authority | The `messaging_effects(logical_id PRIMARY KEY)` pattern in `test/tests/messaging_outbox.rs` records one effect per stable logical ID before success. No actual external business side effects. |

The published `/service` supplies the real registry-to-runtime HTTP lifecycle,
and its retained `/jobs-worker` runs the existing built-in outbox publisher.
The source fixture enqueues a prepared synthetic v1 event through the existing
transactional outbox API and observes publication from each actual A/B/rollback-A
worker; the fixture's typed consumer records the durable logical-ID effect.
This needs no new business registration or image entrypoint. The integration
fixtures also supply controlled historical actors. They remain test-only
executables and are never added to the published image. Reports distinguish
published worker output from fixture consumption and historical-process proof.

Native rehearsal extends the existing PostgreSQL/JetStream harnesses through
their managed endpoints. Source-only `consumer-lifecycle-check` orchestrates
the old/new fixture binaries, provider-native commands and evidence. It does
not provision another runner, embed a backup format, or become a daemon. Its
Compose project, containers, endpoints and volume names are task-specific;
all host port bindings are loopback/ephemeral. Private runtime diagnostics stay
on that network. The original and restored stores coexist only for readback
and are never simultaneously admitted to the same work.

The native recovery target is an explicitly selected, source-only rehearsal,
run once under the task's final-validation owner in the suitable existing local
runtime. It is not multiplied into every profile's ordinary CI suite. The Rust
scenario that requires external old/new executable/archive inputs is explicitly
ignored by ordinary integration-suite discovery and is selected with `--ignored`
by this target; the target verifies execution and completion, so a discovered
but skipped case cannot satisfy D1–D4. Ordinary suites still compile/check the
fixture sources under their existing profile. No permanent restore CI gate or
new runner installation is implied; native CI continues all existing release
proof, and the task retains its actual local recovery receipt separately.

## Ordered operational gates

| Owner/node | Prerequisite and action | Success and distinct safe failure | Horizon, recovery and readback |
| --- | --- | --- | --- |
| Consumer source | Commit pristine baselines and local evolution, accept A→B upgrade with current proof; freeze A and B | Exact full trees, profile choices and review/validation identities present. Missing/ambiguous baseline keeps preparation unaccepted. | No remote effect. Keep original repositories and baseline ancestry until final acceptance. |
| Native CI/CD | Confirm target plan/permissions; run exact-source CI/CodeQL and existing publication action per A/B version tag | Existing build → lifecycle → vulnerability policy → SBOM → push → cosign sign → provenance/SBOM attest → registry verification → tag promotion sequence passes. Wrong subject, identity, issuer, missing attestation or failed readback stops promotion. | Existing publication 45-minute job bound and serialized publication owner. Reconcile run/candidate/tag/digest before retry; retain verified prior digest. |
| Local deployment | Verify A from registry; launch A by digest; migrate with the same admitted image when needed | Readback of source/digest, `app.commit`, dependency admission, readiness and bounded SIGTERM. Local image ID alone cannot satisfy this gate. | Existing hardened flags and 45-second stop grace. Preserve digest/log receipt on partial startup/shutdown. |
| Corrected A→B→A | Admit unchanged persistent contracts, stop/drain A, start verified B, then stop/drain B and start the retained verified A digest | Each distinct revision is observed ready/working and the same durable state remains admitted. A digest equality or schema/handler mismatch fails rollback admission. | Rollback-safe while both corrected binaries support the resulting state. Do not roll back database or broker snapshots as part of image replacement. |
| Historical old→new | On a separate synthetic database, exercise old compatible work; observe new worker refusal before added migrations; apply both additive migrations, then admit new worker | Old worker can use compatible new schema and v1 work during bounded overlap. New worker refuses missing/mismatched history before claiming. | Old retention may run during additive migration. Custody is inactive until every old `Engine::new`/retention owner is stopped. |
| Custody activation | Inventory/stop all old retention owners and observe termination, then enable retained-failure recovery | Corrected worker retains/inspects failed identities; explicit launch/rollback admission refuses an old worker once custody is required. | Keep schema and roll forward. This pair cannot provide successful R3 rollback. Existing lease expiry controls uncertain claims; do not shorten it for proof. |
| Recovery fence | Stop producers, application writes, job claims, outbox publication, message consumption and retention. Join admitted operations or enumerate unresolved identities. | Quiescent controlled fixture owners and durable identity manifest establish the cut. An unknown active writer/claimant keeps capture/admission closed. | Fencing is fixture/operator custody, not a new runtime fleet detector. Record fence/capture times and pending/unknown work. |
| Native archives | Whole-database custom-format `pg_dump`; NATS source and DLQ backups with consumers and configurations; validate/hash archives | Both native commands and validation succeed. Missing/corrupt archive is failed recovery input, not an empty service. | Preserve originals and immutable archives outside disposable-volume cleanup. Record actual server minors and CLI versions. |
| Isolated restore | Provision roles/settings, restore empty database with `pg_restore --single-transaction --exit-on-error`; restore streams into an empty separate broker | Actual data, history, sequence and named consumers read back. Restore/listing success without identity inspection is insufficient. | PostgreSQL rollback-on-error stays local to that restore transaction. Partial NATS restore remains fenced and is retried in a new empty destination from unchanged archives. |
| Reconciliation and readmission | Compare per-ID state/digests; validate migration/queue/history/sequence; invalidate old operator tokens; reconcile effect/intent/source/DLQ/settlement before starting fresh corrected actors | Restored pending work completes, retained failures remain inspectable, and replay outside broker dedupe gives one logical-ID effect. Unexplained missing state or incompatible handlers keep admission closed. | New broker creation identities require fresh startup admission. Existing finite backstops/leases apply. Report fence-to-ready and restore-start-to-durable-completion times, not production RTO. |

## Recovery custody and reconciliation

Capture the exact migration versions/checksums; application/effect IDs; every
rehearsal job ID, kind, payload digest, state, generation and recovery-history
entry; the claim-generation sequence value; outbox logical IDs and publication
state; source/DLQ stream settings and creation identities; message IDs,
sequences, payload/header digests; and durable configuration/ACK/delivery
positions. Native archives include actual payloads only for synthetic data.
Separately retain required roles/grants/extensions and session settings because
a database dump is not cluster-global backup custody.

Independent database/stream/consumer snapshots are not distributed atomic
state. The finite producer manifest plus durable PostgreSQL effect and outbox
records define reconciliation in this drill:

- A recorded durable effect is complete; an identical replay is idempotently
  acknowledged without another effect.
- Committed outbox intent with absent/uncertain publication remains recoverable
  through the existing fenced jobs retry after admission, under the same logical
  event ID. Source presence alone does not prove effect completion.
- A source or DLQ event without an effect remains outstanding and requires its
  compatible registered handler; DLQ redrive follows its existing owner.
- The same logical ID with different payload meaning, an unexplained missing
  committed intent/effect, lost failure custody or inconsistent history is a
  failed recovery boundary. The drill does not invent compensation or discard
  authority.

Restore queue/history/sequence together; the next claim generation must exceed
every persisted generation. Never reset the sequence, truncate history or
silently manufacture a later sequence to excuse an earlier restored history.
Invalidate all saved recovery commands/tokens/receipts, even if their numeric
fields match restored rows; inspect again only after reconciliation. Retain
effect identities for the entire permitted synthetic replay/restore period.

Use NATS CLI native `backup stream` with `--consumers`, `backup validate` and
`backup restore stream`, checking the installed pinned-capability interface
before execution. NATS V2 supports this single-node named-durable case but
does not make snapshot collection atomic or failed restore transactional.
Record changed creation identities, potential replay and expiry/retention
differences. The finite fixture retains data long enough for the drill, and
fails if expiry unexpectedly removes a required identity. No cluster, ephemeral
consumer, older-server V2-reader or failure-zone durability claim is made.

## Proposed external envelope

The following is a concrete proposal for the coordinator to confirm **after**
local candidate preparation, not accepted external authority:

- One repository: `Dankosik/rust-consumer-lifecycle-demo`, proposed **private**;
  main branch `main`, owner `@Dankosik`. No remote minimal-consumer repository.
  Existing GitHub attestation support for private/internal repositories must be
  available through Enterprise Cloud for this native action. If unavailable,
  hold publication and ask the owner about the visibility/account consequence;
  do not make the repository public or weaken verification automatically.
- One image repository: `ghcr.io/dankosik/rust-consumer-lifecycle-demo`; consumer
  release refs `v0.1.0` and `v0.1.1`. Native CD may also publish its documented
  `main`, `sha-<12>`, `latest` and run-scoped candidate tags. The authorization
  request must include those writes, signatures and both attestations.
- Reuse the existing local Docker runtime for digest-addressed `linux/amd64`
  observations. No paid host, domain, public ingress, production data or
  service-to-service traffic. If local architecture/emulation cannot provide
  the actual published-digest observation, return that capability gap before
  choosing another host or image architecture.
- Permissions: create/write only that consumer repository; enable its native
  Actions and required dependency-graph setting; set `ENABLE_GHCR_PUBLISH=true`;
  push its agreed refs; use action-scoped `packages:write`, `id-token:write`,
  `attestations:write`, and the current read permissions. Secret locators only
  enter evidence; no token values or broad personal credentials in fixtures.
- Proposed incremental cash ceiling: **zero**; use only existing included
  quota. Bound consumer execution to two complete version CI/publication
  cycles plus one retry per failed version (four full cycles maximum), with
  existing job timeouts. Read plan/quota availability before dispatch; paid
  overage or exhausted included quota needs matching authority, not a hidden
  runner upgrade. Normal template PR CI remains under its existing authority.
- Proposed retention: native source/CI/release evidence, both verified digests,
  attestations and synthetic archives for **30 days after final acceptance**;
  local runtime at most one source and one restore Compose project during the
  drill. Measure actual registry/cache/temporary disk use before expanding.
  Preserve retained rollback artifacts until the owner accepts cleanup. No
  repository/package deletion or destructive cleanup is implicit; teardown
  removes only named disposable containers/volumes after evidence is secured.

The local preparation receipt presented to the coordinator must contain actual
consumer A/B commits and profiles, render/baseline/lock hashes, compatible
contract comparison, selected local runtime capability and resource inventory,
the proposed external values above, and unresolved effect authority. Source
commits can be prepared before registry digests exist; final digest/readback
fields remain explicitly unobserved until publication.

Repository visibility, account support, spending and retention are the only
remaining user-owned consequences in this proposal. Technical mechanism,
profile, restoration ordering and compatibility admission are closed here.
