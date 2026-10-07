# Consumer lifecycle: evidence for Definition

Status: ready. Valid as of 2026-10-06. Supports [Specification](../spec.md),
not an execution or production-readiness receipt.

## Questions and stopping boundary

Determine whether existing generation/Git/Cargo, native publication, current
integration harnesses, and provider backup tools can support the four accepted
outcomes without a new generic upgrade or backup system. Identify observable
compatibility boundaries, the actual CI bottleneck, and external inputs that
must remain with the coordinator. Stop when Specification and Technical Design
can act without repeating discovery. No new builds, projections, merges,
containers, backups, runtime experiments, or remote writes ran in this phase.

Repository evidence below is from worktree base
`699887b18594088a59bcc23a049d290d089f6da1`, except explicitly pinned PR250
evidence. Primary documentation was checked on the date above. Two read-only
lanes independently gathered upgrade and recovery facts; the Definition owner
checked decision-critical sources and synthesized their implications.

## Existing authority and dependency state

- [Stage 12](../../../docs/roadmap.md#stage-12-first-release-and-derived-repository-verification)
  remains planned. Its first consumer criterion includes minimal and PostgreSQL
  initialization plus an image deployment; it separately retains template
  release/tag/announcement work.
- [Production Contract](../../../docs/production-contract.md) explicitly leaves
  production RPO/RTO, replay/reconciliation, backup custody, mixed-version
  window and rollback authority to the service. A template drill cannot fill
  them with defaults.
- [Portable sync](../../../docs/template-sync.md) owns portable files. Runtime,
  Cargo, image and service-policy files remain outside its overwrite surface.
  The initializer refuses profile/identity migration of an established service.
- PR250 is an admitted prerequisite, not work to duplicate. Its observed CI
  candidate is `c3a3f18752ec8feee03a56a00b0edea2240d3b44`, with source/runtime
  inventory, selected-gate admission and release shapes `1,7,47,65`.
  [CI 37381047660](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37381047660)
  and [CodeQL 37381047558](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37381047558)
  were read back as successful at that head. The coordinator reported the
  separate documentation correction at `57910097357d96da812a56a86c70d973657f70a4`
  and integrated candidate `2cb871895b9edd018205fc98223477e269fce2e9`
  (parents `5791009` and current main `699887b`). That candidate has no claim of
  inherited whole-head CI here; fresh integration review and exact-head CI are
  pending. The coordinator refreshes its admission before dependent implementation.
- This branch already includes the runtime-progress/build-speed work in main.
  Other open runtime work is outside this task; actual dependency changes reopen
  only affected evidence.

## Runtime upgrade evidence

### Available inputs and their limits

`scripts/lib/template_state.py` (`validate_lock`, `load_lock`, committed-tree
materialization) and
[initializer provenance design](../../template-initializer/design/system.md)
establish normalized identity/profile choices and a recorded local source
revision. The revision has syntactic validation, but is explicitly not verified
upstream provenance; it does not guarantee continued object availability.
The lock does not contain a rendered baseline, output hashes, or a history of
runtime upgrades.

`scripts/lib/template_init.py` retains guarded identity rewriting, marker
projection, unselected-profile removal and locked Cargo projection. Replay
checks structural postconditions while allowing ordinary service evolution.
Historical baseline reconstruction is therefore feasible only when the recorded
source, matching helper/inventory, toolchain and generator inputs remain
available. Cheap source projection omits Cargo/rustfmt; public initialization
also performs locked metadata, formatting and OpenAPI generation. Their
byte-equivalence is not established and must not be assumed by Design.

**Counterevidence:** rerunning the current initializer against an evolved service
or treating its current mixed tree as generated output cannot identify which
changes belong to the business. The initialized-output baseline must be obtained
from actual provenance or a reviewed explicit adoption. Existing profile-change
refusal remains applicable.

### Same-level alternatives and complements

| Candidate | Established capability | Fit and remaining gap |
| --- | --- | --- |
| Existing projector + native Git | Three-way tree/text reconciliation, rename and directory/file conflicts, reviewable trees | Reuses the current generation semantics and existing Git dependency; still needs baseline custody, input admission, conflict handling and acceptance ownership |
| Copier update lifecycle | Regenerates old output, computes downstream changes, renders the new template and reapplies changes | Viable maintained alternative, but requires faithful adoption of existing markers/identity rules and historical answers; bridge and baseline equivalence are unproved |
| `cargo-generate` | Liquid/Rhai-based source generation, including in-place/overwrite modes | Generation complement; no evidenced old-baseline reconciliation in its inspected interface. Replacing the established projector does not itself solve upgrades |
| Cargo | Resolves and validates dependency graphs | Complement to either update method; a textual lockfile merge is not graph validation and broad `cargo update` is not a template upgrade |

[Git merge-tree](https://git-scm.com/docs/git-merge-tree/2.49.0) supports an
explicit base and creates a result tree without touching the worktree/index.
The installed Git is `2.50.1 (Apple Git-155)` and its help confirms that mode.
[Git apply](https://git-scm.com/docs/git-apply) offers applicability checks and
three-way patches when original blobs are available; three-way application may
change the index and retain conflicts. `--reject` permits partial application,
which is not the requested atomic preparation contract.
[Git merge](https://git-scm.com/docs/git-merge) warns that abort may not recreate
pre-existing uncommitted work. These facts support isolated preparation and
explicit acceptance, not automatic stash/reset.

[Copier update](https://copier.readthedocs.io/en/stable/updating/) depends on
answers/history and can fail when an old template's renderer or external inputs
are no longer reproducible. Recopy may overwrite local edits. Version
[9.18.2](https://github.com/copier-org/copier/releases/tag/v9.18.2), released
2026-09-07, is MIT-licensed, requires Python >=3.10 and adds Jinja2, Pydantic,
Plumbum and PyYAML among its
[dependencies](https://github.com/copier-org/copier/blob/v9.18.2/pyproject.toml).
No vulnerability audit or adoption trial was performed. Its maintenance is
evidenced; fitness for this existing template is not.

[cargo-generate 0.25.0](https://github.com/cargo-generate/cargo-generate/releases/tag/v0.25.0),
released 2026-09-18, uses MIT/Apache-2.0 licensing. Its
[arguments](https://github.com/cargo-generate/cargo-generate/blob/v0.25.0/src/args.rs)
expose generation/overwrite, not an evidenced reconciliation lifecycle.
[Cargo update](https://doc.rust-lang.org/cargo/commands/cargo-update.html)
distinguishes broad from package-scoped resolution; `--locked` refuses changed
resolution. Preserve that distinction from the initializer's guarded projection.

**Definition decision effect:** U1–U4 require recoverable baselines, reviewable
conflicts, consumer preservation, and separate prepared/accepted states.
Technical Design selects the smallest mechanism. No necessary Rust crate or
reason to replace the existing projector was established. Reopen if baseline
reconstruction cannot preserve the accepted generated contract or native Git
cannot represent a required conflict safely.

## Durable transition and recovery evidence

### Existing contracts

| Surface | Current admission/operation | Constraint on this work |
| --- | --- | --- |
| PostgreSQL migration history | Successful versions above an older binary's maximum are admitted; missing embedded migrations, failed rows, changed checksums and unknown versions within its range are refused | Expand/contract compatibility remains necessary; history admission does not detect manual schema drift or prove business-query compatibility |
| Jobs custody migration | Additive migrations precede corrected workers; old retention owners must stop before custody/recovery activates | Old binaries delete failed work after seven days; an old-binary rollback can restore that behavior. No blanket overlap promise |
| Job kinds/payload versions | Outstanding and restored work needs compatible handlers | A renamed kind or redrive does not migrate payloads |
| JetStream schemas | Handler registration is exact `(type, version)` and unknown versions go to DLQ | Deploy compatible consumers before new-version production; inference: every eligible member of a shared durable must understand the new version |
| Job restore | Queue, history and sequence are restored consistently; effect reconciliation and fresh inspection follow | Pre-restore operation tokens/receipts cease to authorize mutations; a later sequence alone cannot repair older history |
| Replay/effects | Durable logical event identity covers retained failures, streams, DLQ, restore and replay | Broker dedupe and finite TTLs do not cover indefinitely retained/replayable work |

Owners: [Persistence](../../../docs/architecture/persistence.md#migrations),
[jobs upgrade and custody](../../../docs/background-jobs.md#upgrade-and-custody),
[Async recovery](../../../docs/architecture/async.md#operator-recovery),
[messaging](../../../docs/durable-messaging.md), and
[outbox](../../../docs/postgres-transactional-outbox.md). These are existing
contracts, not newly observed runtime results.

### Native mechanisms

The Compose owner pins PostgreSQL 18 and NATS 2.15.0 by digest. Read-only local
capability checks returned `pg_dump`/`pg_restore` 18.3, NATS CLI 0.5.0 with
backup/validate/restore commands, and Docker server 29.4.0. The PostgreSQL
container's actual minor was not observed. No container was started.

For a bounded whole-database synthetic drill,
[PostgreSQL 18 pg_dump](https://www.postgresql.org/docs/18/app-pgdump.html)
provides a consistent logical database snapshot including sequence values;
cluster globals require separate custody. This is not PITR.
[pg_restore](https://www.postgresql.org/docs/18/app-pgrestore.html) normally
continues after errors; error-exit or suitable single-transaction restoration
must make a failed drill visible. A listing is not restore proof.

[Continuous archiving](https://www.postgresql.org/docs/18/continuous-archiving.html)
requires a base backup and complete needed WAL chain, with cluster-level and
configuration custody. That is a different operational contract absent from
the existing harness. [pgBackRest](https://pgbackrest.org/user-guide.html) is a
maintained infrastructure complement for backup/WAL/retention operations, not a
cross-store atomic recovery or effect-reconciliation mechanism. No need for
that additional infrastructure is established for the accepted synthetic drill.

[NATS ADR-63](https://github.com/nats-io/nats-architecture-and-design/blob/main/adr/ADR-63.md)
is implemented for the pinned 2.15.0 V2 snapshot format. It supports file and
memory streams; keeps stream configuration separately; restores to a new stream
with a new creation time; and requires a V2-capable server. Stream and consumer
snapshots are not atomic. Clustered pending acknowledgments can roll delivery
positions back to the ACK floor and lose earlier delivery counts, causing
replay. Expiry/destination retention can change restored contents, and failed
restore is not transactional rollback. These facts bound claims based on the
[generic CLI guide](https://docs.nats.io/learn/backup-recovery/stream-backup-restore),
whose older memory-stream limitation is stale for V2.

Specific counterevidence: [NATS issue 8649](https://github.com/nats-io/nats-server/issues/8649)
reports a 2.15.0 clustered archive with ephemeral consumers that snapshots but
fails validation/restoration. The inspected fix appeared in a 2.15.1-RC.1
[release entry](https://github.com/nats-io/nats-server/releases); no stable fixed
release claim is made. This is not the template's single-node named-durable
case, but prevents extending that local result to production clustering.
[Broker version overlap](https://docs.nats.io/release-notes/upgrade-to-2.15)
has separate supported stepping-stone rules; it does not make V2 archives
readable by older servers.

### Feasibility and limits

The [PostgreSQL harness](../../../docs/validation/postgres.md) already supplies
disposable Compose projects, ephemeral ports and per-test databases.
`scripts/ci/test-integration-messaging.sh` supplies a disposable broker or an
explicit endpoint. Existing proof covers older-history admission, job
identities/history, retained failure, DLQ/redrive and unknown schemas. A bounded
search of scripts/tests/validation owners found no native backup/restore drill
or two-historical-application rehearsal. Those existing tests were not run here.

**Inference:** native tools plus those harnesses can exercise isolated
synthetic dump/restore, broker restore and post-restore operation without a new
backup orchestrator. Identities and effects, not counts alone, distinguish lost,
duplicated and replaced work. The new broker creation identity requires worker
re-admission rather than reusing stale runtime authority.

**Definition decision effect:** D1–D4 require exact version pairs, a supported
stop/overlap sequence, provider-specific limitations, complete recovery custody,
and effect reconciliation. They make no production RPO/RTO or distributed
snapshot promise. Design must choose the concrete native sequence and affected
runtime surface; production topology/data-loss/reconciliation policy remains
an external service input. Refresh on version, snapshot format, topology,
retention, replay policy or backup-provider change.

## Publication feasibility and external input

The current [CD workflow](../../../.github/workflows/cd.yml) admits only the
opt-in same-repository exact-SHA path. Its
[composite action](../../../.github/actions/publish-image/action.yml) builds,
checks lifecycle and vulnerabilities, writes a CycloneDX SBOM, pushes, signs,
attests provenance/SBOM, verifies from GHCR, then promotes tags with readback.
The read-only repository-variable list returned no variables on 2026-10-06;
publication is not enabled by merely having that implementation. No actual
publication or runtime result is asserted.

[GitHub attestation availability](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
is a target constraint for this adapter: current Free/Pro/Team plans support public
repositories, while private/internal attestations need Enterprise Cloud.
The actual native action uses `actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6`
for both provenance and SBOM in addition to `cosign sign`. Its
[pinned README](https://github.com/actions/attest/blob/1e69f48acb82d1966a394da916b4c1698aa569d6/README.md)
confirms attestation API upload and the private/internal plan requirement.
`create-storage-record: false` disables linked-artifact metadata records, not
attestation upload. This is not a general restriction on cosign keyless signing.
Accordingly the proposed consumer's visibility/account support must be resolved
before using this path; it is not a Definition blocker. It must not be silently
made public to bypass that constraint.
[Sigstore verification](https://docs.sigstore.dev/cosign/verifying/verify/)
provides expected certificate identity/issuer checks; the exact repository/ref
and image digest remain required inputs rather than unqualified tag trust.

**Definition decision effect:** preserve the native sequence and independent
registry verification; require two actual distinct verified digests for rollback.
Technical Design prepares a concrete synthetic non-production consumer, target,
visibility, resource/cost/retention envelope and fallback consequences for the
coordinator. No account purchase, new public repository, publication, host or
production-data access is authorized by this artifact. Refresh provider feature
availability and the actual target immediately before an external action.

## CI critical path: measured facts and alternatives

Fresh readback of PR250's
[image job](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37381047660/job/112002891386)
at head `c3a3f18752ec8feee03a56a00b0edea2240d3b44`:

| Observation | Value | Meaning |
| --- | ---: | --- |
| Image job, 2026-10-05 22:13:35–22:54:53 UTC | 2,478 s / 41 min 18 s | Measured native critical-path job |
| Source-image build step | 580 s | Measured step wall time, 22:13:45–22:23:25 |
| Derived-artifact step | 1,830 s | Measured step wall time, 22:23:59–22:54:29 |
| Derived command receipt | 1,828 s | Inner command measurement, not a conflicting job duration |
| Other selected jobs | finished by 22:24:00 | Image job delayed the required aggregate |

The downloaded native `image-proof` artifact ID `11378005825` contains receipt
`attempt.RrBqZi`, SHA-256
`8b46f4b5cfc1a39bbbcf530bd6513d550f54867f1f916e229878bcbdca9eb95a`.
It separates source merge revision
`34ae5a0e1eb9ed8b6700221a3026c5303c5bf308`, private source candidate
`7ee76391922f90d94a4507e7e65d52e4328335e0`, each initialized output revision
and each image ID, and ends `state=passed`. The coordinator verified its 28 log
hashes and five total image identities; this owner read the receipt and the
native job metadata. The retained upstream completion at
`8aec0fe0524d9e096b9eddc962806f851092c146:specs/artifact-compatibility/completion.md`
owns the earlier detailed acceptance evidence. Nothing here re-labels that
evidence as a new execution.

The pinned PR250 runner executes selected `1,7,47,65` sequentially after the
source image, reuses one staged generator target and local BuildKit state, and
imports the source image cache. Derived images do not export four renamed
dependency graphs over that shared source scope. Their run order retains one
loaded derived image at a time. Current Dockerfile stages already parallelize
separate binary links over cooked dependencies. Existing
[build-speed guidance](../../../docs/build-speed.md) and native
`scripts/ci/measure.sh` are reusable measurement owners.

[Docker's GHA cache contract](https://docs.docker.com/build/cache/backends/gha/)
confirms that equal scopes overwrite prior cache objects and branch/event
restrictions affect access. [Build cache guidance](https://docs.docker.com/build/cache/optimize/)
confirms input-sensitive layer reuse and external-cache transfer costs. These
are alternatives to examine within BuildKit, not evidence that another cache
or more parallel runners will improve this workload.

| Alternative | Evidence supporting consideration | Counterevidence / decision gap |
| --- | --- | --- |
| Better reuse/order within existing BuildKit/generator path | Large serial generation/build share; installed mechanisms already exist | Different profile dependencies and renamed package recipes may limit sharing; measure actual hits and affected artifacts |
| Bounded independent image jobs or scheduling | Source and derived proof have distinct identities and are currently serial | Fresh builders may repeat dependencies; runner quota/queue/cache work can erase the gain or increase cost |
| New remote cache product or faster paid runner | Could shift cold build/queue cost | No proven need or cost envelope; native alternatives have not been measured |
| Drop shapes, weaken runtime/security/SBOM gates, change release codegen | Would trivially reduce work | Contradicts the accepted proof-preserving outcome and existing owners |

**Definition decision effect:** C1–C3 preserve selected-gate coverage and require
comparable native timing plus total runner/cache cost. The 90-minute job bound
in PR250 is a limit/forecast, not a user target. No cold/no-cache bound or
numeric improvement target was accepted. Design chooses the supported strategy
after closing the exact dependency and measurement boundary. A changed runner,
toolchain, dependency graph, selected shapes or cache condition reopens a
comparison that depends on equivalence.

## Inputs for the next owner

Technical Design can proceed with closed behavior and native feasibility. It
owns: the exact replayable baseline/target and adoption mechanism; consumer
example/profile and two-version release topology; version pair and recovery
sequence using native formats; evidence custody; and CI optimization choice.
The coordinator owns PR250 dependency integration and the remaining remote
target/visibility/cost authority. These are scoped dependencies, not permission
to replace the accepted outcome with a guide or a passing local suite.
