# Artifact compatibility specification

Status: ready. [Independent Definition review](definition-review.md): PASS. Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Requester authority: [Intent](intent.md). This specification fixes required
outcomes; Technical Design selects mechanisms and ownership, and Implementation
chooses concrete cases and commands.

## Evidence and its limits

The prior read-only research synthesis `synthesis-v1` and independent PASS review
were delivered in the requesting chat; no research files were created. The
coordinator supplied this bounded handoff against the base above:

- [CI 37284599623](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37284599623)
  succeeded; image job `111680427790` built separate auditable locked release
  binaries for service, migrator and jobs worker, observed `app.commit` and clean
  SIGTERM exit. [CD 37286099826](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37286099826)
  was skipped. These are source-template evidence, not a published digest,
  current published SBOM, live provider observation or restore result.
- [Initializer inventory](../../scripts/ci/template-init-check.sh) contains
  runtime representatives plus cheaper projections. Existing workspace/debug
  and focused all-target checks do not prove the selected release binary graph.
- [Railway policy](../../docs/railway-deployment-profile.md) omits `migrations/**`,
  `.sqlx/**` and `vendor/**` from both watch-pattern forms although the
  [Dockerfile](../../build/docker/Dockerfile) consumes them.
- [Delivery policy](../../docs/ci-cd-production-ready.md) has a historical
  byte-identical-build observation; mutable build inputs limit extrapolation.
  Image SBOM/scan tooling exists but does not enforce Rust inventory completeness
  for each retained entrypoint. Auditable data does not retain patch source URLs
  and paths, so it cannot alone identify local patch provenance.

These facts inform the selected behavior below; historical counts and results
must not be presented as fresh execution. Current code/generated contracts remain
canonical. Reopen supporting Research if a decision depends on contrary evidence.

## Required behavior

### 1. Image-changing source changes trigger source deployment

For a derived service using the documented Railway source-build policy, every
repository input affecting the effective Docker build must be covered by its
watch policy. Both the setting table and copy-ready IaC example agree, including
the three omitted input families above. Pruning optional profiles must not leave
an uncovered retained input. A future image-input addition or watch-policy drift
must be rejected by the existing relevant validation route rather than silently
passing. Unsupported or indeterminate input coverage fails closed with an
actionable explanation. Reuse the smallest suitable existing gate or native
coverage source; do not introduce a separate infrastructure configuration carrier.

Nearest falsifier: change only an embedded migration, SQLx metadata or vendored
source, or add an uncovered image input, and observe a policy/check that still
accepts a non-triggering deployment configuration.

### 2. Artifact inventory is complete enough to support its scan claim

For each entrypoint retained in an image (service and, when selected, migrator
and jobs worker), artifact verification establishes a readable Rust dependency
inventory associated with that binary and its selected release graph. Under the
existing trusted auditable build path, verification rejects an absent binary,
missing/unreadable Rust inventory, wrong binary identity or a structurally
incomplete runtime dependency graph; a successful OS-package scan alone is
insufficient. Completeness is structural and graph-relative, not a fixed
package-count threshold. This regression guard does not independently reconstruct
dependencies from machine code or detect an internally coherent omission by the
producer or tampering that preserves those checks; it must not claim that proof.
The resulting SBOM/scan path must expose Rust dependencies from every retained
entrypoint; pruning a binary removes its obligation, not obligations for survivors.

Source revision plus the existing vendored patch custody records identifies
local patches. Do not imply that a package name/version or auditable record alone
establishes unmodified upstream provenance. Preserve existing scan severity,
exception and publication policy; this change closes scan completeness, not a
new vulnerability acceptance policy.

Nearest falsifier: one retained binary loses its Rust inventory while the image
scan still reports success or an SBOM silently contains only another binary's
Rust graph and OS packages.

### 3. Derived release proof covers materially different binary selections

The source template's proof must include a minimal representative set of
initialized/pruned release and image shapes covering materially different retained
binary seams. It must exercise the actual release/package/binary/default-feature
selection used for those deliverables, including required binaries and absence
of pruned binaries. Workspace/debug builds, initializer OpenAPI preflight and
all-target checks alone cannot be reported as this proof. Technical Design
selects the minimum representatives from causal graph differences and states
which selections each represents; it may reuse adequate existing proof.

Preserve the current runtime and projection coverage without creating a Cartesian
profile × harness × infrastructure matrix or repeating database suites. Cheap
projection equivalence remains appropriate for choices that cannot change runtime
inputs. Extend existing CI/initializer/image owners; do not create a parallel
validation framework. Gate failures report the selected graph/artifact boundary.

Nearest falsifier: a retained binary compiles only because unrelated workspace
features are unified, or a pruned generated image still requires an absent crate,
while the representative release/image route passes.

### 4. Initialization and update guidance preserves source identity

Retain locked, offline initialization after an explicit dependency-fetch bootstrap
for a fresh clone. Initializer failure remains fail-closed, preserving its
existing no-partial-target-mutation contract and dependency source/version/checksum
and publish behavior. The public initializer keeps its full preflight.

The current custom lock projection is not automatically a defect. Technical
Design may retain it or replace it with Cargo-native staged resolution only with
evidence that the replacement preserves those contracts and reduces maintenance;
no unproven rewrite is required. Current narrow vendored patches and dependency
versions remain unchanged absent a separately grounded in-scope correctness defect.

[Sync guidance](../../docs/template-sync.md) clearly distinguishes portable
`template-owned.paths` updates from application/runtime ownership (including Cargo,
toolchain, vendor and Docker surfaces outside that portable set), and identifies
`template.lock` as the initial selection receipt rather than current runtime truth.
Replace stale hand-maintained coverage totals with canonical inventory references
or derived values, preserving meaningful descriptions of the proof boundaries.

Nearest falsifier: the documented fresh-clone workflow assumes a populated Cargo
cache without saying so, treats template sync as a runtime upgrade, or a lock
projection changes a selected dependency source/version/checksum unnoticed.

### 5. Rebuild and rollback claims match retained evidence

Guides distinguish reproducible dependency resolution from byte-identical binaries
or image digests. Bound historical observations to their original experiment;
identify remaining mutable package-index/tool/frontend inputs and the manual
retirement responsibility for security overlays. Do not promise that a historical
source checkout remains buildable forever or force snapshot infrastructure.

For image deployment, retain and verify the accepted immutable digest as primary
rollback identity. For Railway source deployment, retain the successful deployment
identity and source revision and distinguish redeploying that retained artifact
from rebuilding old source. Both paths pair application artifact and compatible
configuration; startup identity/readiness checks do not prove schema or payload
compatibility. No new provider setup or automated rollback is introduced.

Nearest falsifier: an operator follows the guide and treats a newly rebuilt old
commit or mutable tag as the previously accepted artifact, or assumes historical
binary equality establishes all future rebuilds.

### 6. Rolling update and rollback guidance closes compatibility boundaries

Consolidate guidance in existing delivery, persistence and capability owners:

- Use expand-before-contract schema changes. A migrator admitting newer
  successful history does not prove old SQL or application data compatibility.
  Destructive contraction waits until incompatible readers/writers and relevant
  rollback/restore needs are retired under service policy.
- Explain direct/session-compatible migration and LISTEN endpoints separately
  from transaction pooling and the existing polling fallback. Recycle warmed
  PgBouncer prepared statements only when a relevant schema/result-shape change
  requires it; do not require RECONNECT for every migration.
- Deploy compatible consumers before publishing new event/job payload versions.
  Every old replica that can receive the new shape must handle it, or routing,
  filtering or durable separation must prevent that delivery. Keep kind/payload
  versions stable for outstanding and restorable work.
- Pair config rollback with the binary because unknown fields are rejected.
  Preserve the [jobs retention warning](../../docs/background-jobs.md#upgrade-and-custody):
  retaining failures requires every old seven-day retention owner to stop or be
  replaced. Keep additive schema and roll-forward recovery guidance; do not
  duplicate or weaken that existing custody rule.

The service owns its compatibility window and retirement criteria; this PR adds
no runtime compatibility layer or migration. Nearest falsifier: a rolling plan
satisfies the guide yet lets an old receiving replica reject a new payload, an
old retention owner delete promised failures, or an old binary reject new config.

### 7. Recovery guidance makes independent-store limits explicit

Use existing service production/recovery owners to describe responsibilities:
PostgreSQL managed backups/PITR or dump/restore plus roles, extensions, migration
history, sequences and secret custody; JetStream native snapshots including
consumer state for source and DLQ streams plus separate identity/config backups;
object-store data/version recovery distinct from database recovery. DLQ redrive
is not snapshot restoration. Use guidance compatible with the pinned provider
versions, not new CLI features unavailable to them.

The current latest-key object API does not select a historical VersionId.
Explain immutable keys/digests or a service-owned need for version-aware access
without adding a generic versioning feature. Cache normally invalidates on
recovery unless a derived service deliberately makes it authoritative.

Independent stores have no coordinated snapshot guarantee: older PostgreSQL with
newer broker state can lose dedup history and duplicate effects; newer PostgreSQL
with older broker state can lose events already recorded as published; old database
references plus overwritten object keys can retrieve wrong bytes. Restore into
an isolated/fenced context, reconcile the service's stores and identities before
resuming claims/writes, and invalidate recovery commands prepared before restore.
Do not imply reconciliation can universally reconstruct missing data.

RPO, RTO, backup custody/retention, dedup horizon, reconciliation policy and observed
restore proof remain explicit derived-service responsibilities in the
[Production Contract](../../docs/production-contract.md). This PR supplies guidance,
not live recovery execution or a claimed universal guarantee.

Nearest falsifier: the documented procedure resumes writes after restoring only
PostgreSQL without addressing newer broker/object state, or calls an unexercised
backup plan proven recovery.

## Unchanged behavior and proof boundary

No REST/runtime/config/schema contract, dependency/toolchain version, provider
resource or business retention value changes. No production mutation, merge,
deployment, generic backup orchestration or historical bitwise-build requirement.
Existing TLS/native-dependency choices and narrow SQLx patches remain in place.
Template source CI, a derived graph, a published digest and a live provider remain
separate evidence scopes. Required checks follow repository validation ownership;
CI-owned heavy proof stays in CI, with no duplicate full local matrix.

Definition completes after independent Specification Review. Technical Design
must close gate integration, inventory completeness mechanism, minimum derived
release/image representatives and lock-projection disposition before Planning.
A discovered incompatibility changes the smallest affected owner; unrelated
research recommendations do not silently expand this PR.
