# Artifact compatibility design

Status: ready. [Independent Technical Design Review](review.md): PASS.
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Authority: [Intent](../intent.md), [Specification](../spec.md).
This is the Technical Design result; implementation and CI evidence remain
pending. No runtime, schema, toolchain, dependency or provider change is selected.

## Decisions and material flows

### Source deployment coverage

Keep the Railway policy in its existing document. The setting table and IaC
snippet must contain the same watch set. Besides their current entries, cover
`migrations/**`, `.sqlx/**`, `vendor/**`, `test/Cargo.toml` and `test/src/**`:
the last two enter the current Docker context even though tests are not built.
Retain the conservative `rust-toolchain.toml` trigger because it is coupled to
the builder pin; do not claim the file itself enters this context.

Add one small stdlib Python check, `scripts/ci/image-inputs-check.py`, to the
existing `docs-check` target before lychee. It reads the two policy forms,
the effective Dockerfile and ignore file, and refuses missing/ambiguous forms,
disagreement or uncovered input families. It validates policy as data and never
executes the IaC example. Route Dockerfile, ignore-file and checker changes to
`documentation` too, so both CI and `make verify` run the same target. A policy
text edit therefore needs no Rust build or image build merely to check coverage.

For the current repository pattern, use conservative coverage rather than a
second general Docker engine: the root ignore file begins by excluding
everything and explicitly re-includes literal file/directory families. Require
every positive family to be watched (directory as `path/**`); exclusions can
only reduce that obligation. Include the Dockerfile and active ignore file as
build controls even when absent from the context. Validate that build selection
uses this root context and Dockerfile. Reject a Dockerfile-specific ignore
override, named/local alternate context or unsupported inclusion/watch syntax
until this narrow check understands its coverage. Diagnostics name the exact
input or unsupported syntax and its policy owner. This covers a newly admitted
family before any file in it exists, unlike enumerating today's tracked files.

Docker's own context semantics remain authority; BuildKit `--check` does not
compare Railway's documentation to context inputs. A full Dockerignore/glob
implementation or additional provider configuration carrier would add a second
owner without improving this repository's supported case. The accepted cost is
conservative deployment triggers; reopen for an actual different build-context
shape, rather than silently approximate one. Existing canonical profile
projections run the checker on projected trees to prove retained inputs remain
covered and the policy forms stay equal after pruning.

Flow: image-input or policy edit -> shared classifier -> `docs-check` -> policy
coverage PASS, or actionable refusal before accepting the guide. The source
deployment provider is not called and no deployed settings are certified.

### Per-binary Rust inventory

Keep cargo-auditable 0.7.6 and Trivy 0.74.0. The Dockerfile's separate
`cargo auditable build --release --locked -p <package> --bin <binary>` calls,
with their current default-feature selection, remain the producer of embedded
metadata. They associate the inventory with the individual compiled binary.
No workspace/all-features build or whole-lockfile comparison substitutes for it.

At the existing container scan/SBOM boundary, resolve the local image tag to an
immutable image ID once and scan that ID with pinned Trivy JSON output and
`--list-all-pkgs=true`. The repository's retained Cargo package/binary targets
and profile selection determine the expected `/service`, optional `/migrate`
and optional `/jobs-worker` set independently of Trivy's results. Service root
identity comes from the current manifest, including inherited version; worker
retention means jobs **or** messaging. Cross-check those expected outputs with
the projected Dockerfile; ambiguous/missing identity refuses. Do not let the
set of discovered scan targets define its own obligations.

`scripts/ci/runtime-image-inventory.py` owns only native-report admission. For
each expected executable, require one matching `Class=lang-pkgs`,
`Type=rustbinary` result at its normalized absolute target path, with:

- nonempty, unambiguous package IDs and names/versions;
- exactly one `Relationship=root`, matching the selected package/version;
- non-root runtime dependency content reachable from the root;
- every `Packages[].DependsOn` reference resolving within that result, and
  every reported package reachable from the root using a visited traversal.

Unexpected known pruned entrypoint results fail too. Binary presence/absence is
also checked from the image filesystem using the existing Docker create/copy
pattern, including pruned binaries that lack any Rust metadata. Missing binary,
unreadable metadata, root-only data, dangling references or disconnected content
cannot pass as a clean OS scan. No fixed package count is used. The report's
normal runtime graph is the inventory scope; build-only/proc-macro packages are
not falsely required in that representation.

Use `trivy convert` on the admitted native JSON to emit CycloneDX, and native
conversion/reporting for the existing vulnerability verdict and readable output.
Preserve fixable HIGH/CRITICAL admission, existing ignore behavior and publication
order. The JSON must retain all package information before severity/report
filtering. Validate before conversion because Trivy's SBOM encoder skips dangling
references. Conversion must retain each binary's application component and its
dependency graph, including shared packages referenced by multiple binaries.

The existing `container-security` and `container-sbom` targets own invocation;
a small shared shell helper may own the repeated Trivy Docker command and
temporary-file cleanup. Do not add a custom SBOM schema, ELF decoder, scanner or
dependency. Generate reports into private temporary paths and publish a named
output only after admission/conversion succeeds. Each invocation scans its fixed
image ID; sequential security/SBOM calls may re-scan with Trivy's analysis cache,
avoiding an unvalidated cross-command report cache. Publication uses the same
built candidate before push; a failed gate prevents push/sign/promotion.

This is structural completeness of the trusted producer's reported runtime
graph. A coherently falsified producer graph cannot be detected by its own
metadata, and neither package membership in Cargo.lock nor all-features
metadata is an independent actual-compilation oracle. Local patches remain
identified by source revision plus existing vendor custody records, not the
auditable package's upstream-looking name/version. Reopen this decision for an
observed producer omission or changed native report semantics; do not claim
machine-code equivalence or malicious-build protection.

The strongest smaller alternative, checking only a nonempty `rustbinary`
package list, admits a root-only or broken graph. Additional native tools such
as rust-audit-info/Syft duplicate extraction already supported by the pinned
Trivy. The small validator is application admission policy over a native output,
not an implementation of the underlying format.

### Minimum derived artifact proof

Reuse the canonical tuples in `template-init-check.sh --list-graphs`, one `core`
harness, the public initializer's full preflight and the existing image targets.
Add an artifact-only mode to that source runner; it initializes selected copies
and proves their images without repeating their debug/workspace/database suites.

| Existing graph | Retained image entrypoints | Causal seam |
| --- | --- | --- |
| 1 | service | All optional binaries removed; renamed service and minimal dependency closure |
| 7 | service, migrate | PostgreSQL retained with worker removed; migration/SQLx/vendor context |
| 47 | service, jobs-worker | Messaging-only worker without PostgreSQL/jobs/migrator |
| 65 | service, migrate, jobs-worker | Initialized full neighboring capability set, including default release features and native dependencies |

These are the four distinct retained-binary selections. Graph 65 also exercises
the initializer's full selected engine (introspection) and transformed identity;
the existing source image continues to prove the uninitialized source selection.
The source image cannot replace a derived image's marker removal, lock projection,
identity rewrite and projected Docker stages. Other profile/harness alternatives
keep their existing runtime/projection coverage; they do not multiply artifact
or database suites. Reopen the set when a new binary, binary-retention predicate
or independent artifact-build seam appears.

Run selected derived images serially in the existing CI image job, retaining its
source image step only when the source `runtime_image` surface requires it. Extend
the existing initializer matrix planner to emit a bounded artifact graph list:
runtime-affecting paths select retained representatives using its existing
profile-removal logic; Dockerfile/ignore/build/inventory/initializer-wide inputs
select all four. Harness-only or prose-only changes remain projections. The
image job runs when either its source image or a derived artifact is selected;
the existing required aggregate refuses a failed/cancelled selected image job.
Use that same selection in `make verify`, as CI-owned heavy proof.

Within the job reuse BuildKit layers, the shared staged OpenAPI Cargo target and
Trivy cache. Build each selected image once through `runtime-image-build`, then
use the same image ID for filesystem/identity/inventory, lifecycle and security/
SBOM checks. Docker's release builds themselves prove exact package/bin/default
feature selection; do not add a duplicate host release build. Existing worker
lifecycle logic already admits jobs OR messaging and checks default refusal
without a provider; preserve it. Add migrator filesystem presence/absence proof
without running migrations here. Normal service readiness and stop proof remain
in the existing helper; no database/broker/provider startup is introduced.

Use the existing command recorder to retain source candidate, initialized
revision/lock identity, graph ID, selected image identity, command result and
failure log. A failure stops the sequence; it names the graph and artifact gate.
No historical CI result is relabelled as proof of these new derived images.

### Initialization and source identity

Retain `_project_optional_feature_edges` and the guarded lock projector. Its
version/source/edge anchors deliberately refuse an unfamiliar graph before
target writes; locked offline Cargo metadata plus formatting and OpenAPI
generation remain the public initializer's complete preflight. Preserve source
versions/checksums, local patches, package publish settings and target mutation
semantics. This work does not change the projection algorithm.

The strongest native replacement is staged `cargo update --workspace --offline`
with a before/after identity check, followed by locked validation. Cargo may add
packages absent from the supplied lock; `generate-lockfile` can resolve newer
packages, and `--locked` refuses a changed resolution. Replacing the projector
therefore needs a deliberate staged resolution policy plus identity guards and
proof of offline/cache/publish/refusal parity. No current correctness defect or
bounded parity evidence makes that cost preferable here. Reopen on a concrete
unsupported graph or measured maintenance burden with a demonstrated simpler
identity-preserving replacement. Do not run unlocked resolution as validation.

Update `docs/template-sync.md` and the initialization entry in README to show
`cargo fetch --locked` as the explicit connected fresh-clone bootstrap before
offline initialization. A failed bootstrap/preflight remains a refusal, not a
partial initialized target. Clarify that `template-owned.paths` is portable sync
authority; Cargo/toolchain/vendor/Docker and application changes outside it need
service-owned adoption/review. `template.lock` is initial selection provenance,
not current dependency versions or runtime configuration. Replace stale counts
with canonical runner/projection inventory references, retaining proof scopes.
No automatic runtime upgrade, profile migration or portable ownership expansion.

## Operating guidance ownership

This PR changes the existing guides, not deployment procedures or runtime logic.
No standalone recovery platform or task-local rollout is needed.

| Existing owner | Change and boundary |
| --- | --- |
| `docs/ci-cd-production-ready.md`, Dockerfile comments, `docs/validation/containers.md` | Bound historical cold-build equality to its experiment; distinguish lock resolution from binary/image equality. Name mutable APT indexes/packages, Docker frontend tag and security-overlay availability/retirement. Explain each retained binary's inventory, replacing stale fixed counts and jobs-only descriptions. Source CI is not published-digest proof. |
| `docs/railway-deployment-profile.md` | Policy coverage above; retain accepted image digest or retained source-deployment identity plus revision, with compatible config. Distinguish retained-artifact redeploy from old-source rebuild and verify identity/readiness without claiming schema/payload compatibility. |
| `docs/architecture/persistence.md` | Expand before contract; newer successful migration history does not prove old SQL/data compatibility. Contraction waits for old readers/writers and rollback/restore needs to retire. Preserve direct/session-compatible migration and LISTEN connections and polling fallback. Recycle prepared statements only for relevant result/schema changes. Add PostgreSQL backup/restore scope: data, roles, extensions, history, sequences and secret custody. |
| `docs/durable-messaging.md` and `docs/background-jobs.md` | Compatible consumers precede new payload production; every receiving old replica must understand the new shape or be excluded by routing/filtering/durable separation. Retain handlers for outstanding/restorable kinds. Link the existing jobs retention custody rule unchanged. Distinguish DLQ redrive from native JetStream snapshot restore; source/DLQ stream snapshots include consumer state and need separately retained broker identity/config. |
| `docs/configuration-source-policy.md` | Briefly link binary/config rollback compatibility: unknown fields are refused; secrets retain existing custody and are not copied into examples. |
| `docs/object-storage.md`, `docs/cache.md` | Object data/version recovery is separate from PostgreSQL; latest-key reads cannot select VersionId. Immutable keys/digests or a service-owned version-aware requirement close overwritten-key risk. Cache invalidation is the normal recovery choice unless the service explicitly makes it authoritative. |
| `docs/production-contract.md` | Add explicit service-owned RPO/RTO, backup custody/retention, dedup horizon, reconciliation and observed restore proof fields. Own the cross-store warning and fenced recovery sequence, with links to the capability guides. |

Cross-store flow: fence producers/claims/effects -> restore chosen retained
artifacts, compatible configuration and independent stores into isolation ->
reconcile identities, dedup/publication state and object references -> invalidate
commands prepared before restoration -> service owner admits resumed writes.
Older database/newer broker can repeat effects; newer database/older broker can
lose already-published events; restored references/overwritten keys can retrieve
wrong bytes. Reconciliation cannot promise reconstruction of missing data. Any
unresolved store/custody mismatch keeps writes fenced; do not turn readiness into
a recovery success signal. Service policy owns timing, retirement and admission.

## File ownership and validation integration

No Rust crate or module placement changes. Existing owners mechanically fix the
map; a separate Rust ownership artifact adds no decision.

- `scripts/ci/image-inputs-check.py` owns policy/input admission only;
  `scripts/ci/runtime-image-inventory.py` owns native report admission only.
  Any shared shell invocation helper stays beside existing image scripts.
- `make/template.mk` composes checks; `scripts/ci/changed-surfaces.sh`,
  `scripts/ci/verify.sh` and `.github/workflows/ci.yml` select the same gates.
  `.github/actions/publish-image/action.yml` consumes the strengthened existing
  security/SBOM targets before its existing publication effects.
- `scripts/ci/template-init-check.sh`, `scripts/ci/initializer-matrix.py` and
  `scripts/tests/template-profile-projections.py` own canonical artifact tuple
  reuse, CI selection, and cheap projected policy coverage respectively.
- New portable helpers used by portable Make targets join `template-owned.paths`;
  the initializer's source-only runner remains source-only. New paths join the
  existing candidate-path inventory where required. This changes tooling custody,
  not Cargo/vendor/Docker ownership. Preserve profile markers and sync purity.
- Existing self-test/fixture patterns near these scripts own focused gate
  falsifiers. Implementation chooses cases and commands under `test-audit`,
  including routing parity and negative native-report/policy inputs; there is
  no pre-implementation test plan or per-unit review gate.

Final validation is assembled once under repository routing: scoped script and
native-format checks, documentation links, classifier/verify/planner self-tests,
profile projections, shell/workflow checks for their changed surfaces, then
selected heavy image/initializer proof in CI. No local full aggregate, duplicate
database matrix, profiling or live recovery is required. New native output
assumptions must be exercised by that CI proof before claiming implemented scan
coverage; this design itself claims source-backed feasibility only.

## Primary evidence and reopen conditions

- [Trivy 0.74 Rust parser](https://github.com/aquasecurity/trivy/blob/v0.74.0/pkg/dependency/parser/rust/binary/parse.go)
  and [native application conversion](https://github.com/aquasecurity/trivy/blob/v0.74.0/pkg/fanal/analyzer/language/analyze.go)
  expose runtime packages, root relationship and dependency edges.
- [Trivy native conversion](https://github.com/aquasecurity/trivy/blob/v0.74.0/pkg/commands/convert/run.go)
  and [SBOM encoder](https://github.com/aquasecurity/trivy/blob/v0.74.0/pkg/sbom/io/encode.go)
  support reuse of admitted JSON and motivate validation before conversion.
- [cargo-auditable 0.7.6 collection](https://github.com/rust-secure-code/cargo-auditable/blob/v0.7.6/cargo-auditable/src/collect_audit_data.rs)
  and [graph projection](https://github.com/rust-secure-code/cargo-auditable/blob/v0.7.6/cargo-auditable/src/auditable_from_metadata.rs)
  are the producer association, not an independent machine-code oracle.
- [Cargo update](https://doc.rust-lang.org/cargo/commands/cargo-update.html)
  and [lock generation](https://doc.rust-lang.org/cargo/commands/cargo-generate-lockfile.html)
  explain why native staged resolution still needs an identity contract.
- [Docker context](https://docs.docker.com/build/concepts/context/) owns ignore
  precedence and input semantics; the check deliberately admits the repository's
  bounded allowlist form and refuses unsupported coverage questions.

Reopen the smallest design owner for a native JSON shape incompatibility,
legitimate graph rejected by structural rules, new binary retention seam or
unsupported context grammar. Reopen Specification only for changed desired
behavior/authority, and supporting Research for contrary provider/tool evidence.
The prior source CI and skipped CD remain bounded historical evidence from
Definition; no published SBOM or restore proof is added by this phase.
