# Specification: template initialization and portable sync

Status: ready. Definition owner: stage9_definition. Baseline:
`81bbd16b320c90d430522e69fc2d52a2aaa049de`.
Requester meaning: [Intent](intent.md). Evidence: [research](research/synthesis.md).
This contract closes behavior; Technical Design owns mechanisms and placement.

## Outcome and supported selections

A new service can be initialized from this buildable Rust checkout, then adopt
portable instructions/tooling from a committed template without replacing its
service-owned content. Stage 9 adds no runtime capability.

`make template-init` accepts `SERVICE_NAME`, `REPOSITORY`, `DESCRIPTION`,
`CODEOWNER`, `DATABASE`, and `AGENT_HARNESS`. Identity fields are required;
`DATABASE=none` and `AGENT_HARNESS=all` are the defaults. The direct script entry
must express the same inputs and reject unknown or conflicting arguments.

| Selection | Observable result |
| --- | --- |
| `DATABASE=none` | HTTP core remains; PostgreSQL provider/migration code, DB-only dependencies, configuration, tests, image payload, compose resources, and executable DB gates are removed. |
| `DATABASE=postgres` | Existing PostgreSQL profile remains complete and runtime-inert until configured with `postgres.enabled=true`; no credential or network is invented. |
| `AGENT_HARNESS=core` | Canonical `.agents` skills/roles and AGENTS.md remain; no product-specific carriers or generated views remain. |
| `codex`, `claude`, `qwen`, `cursor`, `grok`, or `opencode` | Canonical sources plus exactly the selected existing adapter's carriers/views remain. |
| `AGENT_HARNESS=all` | All six current adapters remain. |

All 16 combinations are supported. Arbitrary adapter subsets, unknown values,
and stage-10 options are refused before mutation, never silently ignored.
Core HTTP, health, telemetry, configuration, Rust toolchain, crate boundaries,
and existing validation standards stay unchanged. A bare initialized service
starts without external services for either database selection.

## Identity and initialization state

`SERVICE_NAME` is a lowercase ASCII Cargo-compatible name: starts with a letter,
then letters, digits, or single hyphens, ends in a letter/digit, at most 64
characters. Reject names colliding with an existing non-service workspace
package or auxiliary binary and Rust reserved identifiers (including their
underscore crate spelling). The implementation exposes one deterministic
validation rule used by both entry points. `REPOSITORY` is an HTTPS GitHub
repository URL with exactly owner/repository path components, no credentials,
query, fragment, or trailing slash. `CODEOWNER` is one `@user` or `@org/team`
GitHub owner token. `DESCRIPTION` is nonempty single-line text, at most 256
Unicode characters, without control characters; quotes and punctuation are
data and are escaped for every destination.

Success updates the service package and executable name, its description,
workspace repository identity, CODEOWNERS active owner rules, README service
identity/onboarding, runtime service identity defaults, command/image inputs,
OpenAPI title/description source and committed projection, and tests that name
the executable/package. The `crates/service` directory and non-service package
names remain stable. Explicit upstream provenance references are retained and
must not be mistaken for stale service identity. No unrelated text replacement.

Initialization requires a Git repository root and a clean tracked checkout;
untracked/ignored content inside its planned write/removal scope refuses it,
and unrelated untracked/ignored content is preserved. The initializer preflights
all inputs, required source shapes, profile-marker balance, required tools,
path types and symlinks, and the full transformation before changing the target.
Unknown/duplicate/mismatched markers or missing required identity anchors refuse
rather than produce a plausible partial service. Selected blocks retain content
and lose marker lines; unselected blocks and owned pack files disappear. No
profile markers remain on initialized executable/configuration/documentation
surfaces; literal syntax examples in the initializer's own tests/docs are allowed.

`template.lock` records a versioned schema, selected identity, database and
adapter choices, source identity, the actual local checkout revision at the
start, and initialization state. Local checkout provenance never masquerades
as a verified upstream revision. A completed lock means only that initialization
postconditions passed; it is not build, test, CI, or deployment evidence.

The initializer is one-shot: with a complete matching lock, a repeat checks
identity/profile postconditions and returns a no-op success without restoring
service-owned edits; differing choices refuse. An incomplete lock or inconsistent
postconditions refuse with a recovery diagnostic. It does not resume destructive
work automatically. A failed write never leaves a newly completed lock or prints
success. Unexpected I/O/lock-generation failures may leave init-produced local
changes, explicitly reported; recovery uses review of that diff and a fresh
clean template checkout. No automatic reset, clean, stash, stage, or commit.
Changing profiles in an established service is a separate future operation.

The output includes a compatible Cargo.lock deliberately transformed during
initialization; retained external package versions/checksums are unchanged.
All subsequent Cargo validation uses `--locked`. Renaming or removal cannot
require an ordinary build to update the lock implicitly.

## Profile closure and repository-owned truth

Removal closes the entire dependency and command graph: no missing package,
feature, image COPY/build target, generated contract owner, test fixture, config
reference, selected CI dependency, or documentation link may remain active.
Generic architecture/reference guidance may remain only when it clearly names
profile availability and its referenced local authority exists. It must not
advertise an absent operation as runnable. Unselected profile packs cannot be
resurrected by a later portable sync. Existing runtime lifecycle and PostgreSQL
failure semantics are preserved in outputs retaining that profile.

`template-owned.paths` is the sole full-sync ownership manifest: relative safe
paths, one per line, directory ownership explicit, comments/blank lines allowed.
No absolute path, traversal, ambiguous spelling, duplicate/overlapping owner,
symlink/submodule, empty owner, or missing source owner is admitted. Directory
ownership mirrors descendants including deletion of target-only owned content;
removing a manifest entry stops future ownership and leaves the target path.
Standalone script files are listed individually to protect service siblings.

Owned bytes must be portable across all initialized combinations: no service
identity, deployment target, owner, service-specific invariant, or profile
marker. Generated projections are rebuilt by canonical source helpers instead
of becoming a second authority. Template-owned sources include the manifest,
synchronizer and its necessary helpers, portable Make entry/implementation,
workflow and harness methods, and other demonstrably portable tooling. Exact
manifest inventory is a Technical Design result subject to these constraints.

The following remain service-owned and are never copied, deleted, or replaced
by sync: runtime/application source, Cargo manifests/lock, config and secrets,
OpenAPI, migrations, README, CODEOWNERS, template.lock, service Make data/recipes,
architecture/configuration decisions, project structure, development-command
and CI/deployment policy documents, workflow activation, and code-policy
exceptions. Split generic method from local fact when necessary; portable
instructions must link to the local owner. Template-only roadmap, acceptance
bundles, initializer matrix, purity and sync fixtures stay source-owned.

For service additions inside `.agents/skills`, a `.service-owned` marker plus
a real SKILL.md reserves that whole directory regardless of dirty status.
It must not collide with a source template skill. Malformed/colliding markers
refuse; unmarked target-only skills remain manifest-owned drift and are deleted
by apply. Applicable generated discovery links for marked skills are preserved
or regenerated locally; they do not turn local skill content into template data.

Claude settings own only `env.CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH`; Qwen
settings own only `model.maxSubagentDepth`. Other JSON is consumer-owned and
preserved byte-for-byte outside the changed managed leaf. Missing parent
objects/files may be created. Malformed JSON, duplicate keys, non-finite
numbers, or non-object document/managed parent refuse without printing values.
Codex config remains the exact generated portable project view as its existing
owner requires; machine-specific settings remain outside project config.

## Sync contract and refusal precedence

`scripts/template-sync.sh --check|--apply [--instructions-only] --from SOURCE
--repo TARGET` uses one committed SOURCE HEAD snapshot and validates its
manifest/canonical inputs/generated projections before target mutation. SOURCE
and TARGET must be distinct Git roots; no fetch or remote write is implicit.
Dirty applicable source owners or canonical helper inputs refuse both modes.
Unrelated source dirt does not enter the snapshot. Snapshot identity is printed.

`--check` is read-only: exit zero means selected owned bytes, modes, removals,
and generated views match; nonzero distinguishes drift from refusal in its
diagnostic. `--apply` applies that same comparison's changes and leaves them
uncommitted, reporting success only after selected content matches. A following
check reports clean after the operator commits the sync changes; running check
while owned changes remain dirty correctly refuses even when bytes match.
An already-current clean target is a no-op. Full sync preserves adapter choice
from the complete supported template.lock and prunes unselected adapter paths.

`--instructions-only` selects portable bootstrap/docs/skills/roles and selected
adapter configuration/views. It preserves Makefiles, scripts, tool pins,
manifest, receipts, and unselected adapter data. It invokes committed source
helpers where necessary and never target Makefile code. Dirty unselected tooling
and legacy Makefile structure do not block it. Success claims instruction
parity only. Both modes preserve service-owned content and unrelated dirt.

Before the first target write, both modes reject every applicable condition:

- Missing, malformed, unsupported, or incomplete template.lock; invalid input
  mode/selection; missing required service-owned authorities referenced by the
  selected portable instructions.
- Tracked changes, staged changes, untracked or ignored content in selected
  owned/generated/pruned paths, except valid marked service skills and their
  valid generated links. Never stage, stash, reset, commit, or erase this work.
- Unsafe manifest paths, symlinks in any traversed source/target owner or parent,
  submodules, file/directory/type collisions, ignored destination paths, or
  ignored source content in applicable owned paths. Generated skill links are
  allowed only in canonical shape pointing to matching local skill owners;
  their roots cannot be symlinks or contain unmanaged real files/directories.
- Invalid source projections, malformed Codex managed markers, unsafe settings
  objects, missing helpers/tools, or owned content containing the target's
  repository identity. Diagnostics show safe path/reason, never settings values.
- Full sync against an unsplit legacy Makefile containing service recipes, or
  standard-target overrides in the service extension. Service recipes must be
  moved to the supported service extension before full sync; no forced mode.

Refusal leaves target bytes and Git state unchanged. Unexpected filesystem or
helper failure after admitted writes reports failure and may leave sync-produced
changes for review; never false success or automatic destructive rollback.
Concurrent edits during a command are unsupported: commands require exclusive
access to their selected paths and document that boundary; initial dirty checks
do not claim transactional isolation from another process. Source/target content
and filenames are data, never shell expressions or executable configuration.

## Purity, CI, documentation, and proof

`make template-owned-purity-check` verifies structural manifest safety,
repository-owned exclusions, source existence/nonemptiness, absence of profile
markers and template identity contamination, and propagation of the sync
mechanism. Semantic portability and valid local-authority links additionally
receive static review; a string scan alone cannot prove them.

`make template-init-check` exercises all 16 supported combinations from the
fixed candidate through initialization, `make build`, and the real `make check`
without bypassed constituents. It also covers identity/lock, removal, generated
views, repeated initialization, unsupported choices, and refusal preservation.
The source initializer matrix is not recursively invoked by a generated
service's make check. CI selects the matrix through `module_initializer`,
including identity/profile owners and initializer/sync tooling. The required
aggregate must require terminal success for a selected matrix; no selected
skipped job counts as proof. Existing gates remain intact and unavailable
mandatory tools are failures, not passes. Local execution uses existing full
and heavy opt-ins and serial validation locks.

Sync fixtures prove clean committed-source adoption, zero drift after committing
the result, identity/local-content preservation, directory deletion semantics,
selected adapters, settings and marked skills, full/instruction-only distinctions,
and meaningful refusals with unchanged target snapshots. A canary derived Git
repository provides end-to-end local evidence. Runtime PostgreSQL changes, if
introduced by removal wiring, retain the matching real-DB evidence required by
Validation budget; pure file-safety claims need no database.

`docs/template-sync.md` owns usage, prerequisites, ownership, modes, refusal and
recovery behavior, and the local-versus-CI evidence distinction. README and
command/validation owners describe actual initializer/profile support. New or
materially changed skills receive positive/negative fixtures under evals and
neighbor review; instruction fixture evidence is labeled static. No new profile
skill or universal discipline is required unless Design demonstrates reached
pressure under the [research disposition](research/synthesis.md#stage-7-and-harness-obligations).
If Planning selects a ledger, its first real dispatch/return/landing must be
recorded against the stage-8 carried obligation.

Stage 9 becomes done only when all supported outputs and sync canary meet these
exit criteria on main. Local matrix execution proves those commands for the
fixed candidate; merely configuring GitHub Actions is not a remote CI run.
No push, merge, deployment, release, or stage-10 work is authorized here.

## Outcome, necessity, composition, and reopen

Outcome falsifier: a generator that renames README but leaves package/image
identity stale, a no-DB service whose standard check invokes migrations, or a
sync that overwrites architecture can pass shallow checks yet fail the request;
all are explicitly forbidden above. Added safety and preservation rules trace
to the Go rationale and AGENTS authority; optional future capabilities are excluded.

Representative composition: initialize `catalog-api`, PostgreSQL absent, Claude
only; retain a service-owned skill and custom Claude settings; commit service
work; adopt portable instructions while tooling is dirty; then commit the
instruction result. Instruction check succeeds without changing tooling or
restoring PostgreSQL/other adapters. Full sync refuses dirty tooling until its
owner reconciles it, then adopts portable tooling while preserving service facts.

Reopen Intake for changed outcome or authority, Research for capability/source
or mechanism evidence, Specification for changed supported selections or
preservation semantics. Technical Design next closes transformation ownership,
lock schema, snapshot and safe-write mechanism, portability refactors, profile
inventory, and non-recursive matrix integration without changing this behavior.
