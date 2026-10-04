# SQLx backport source and delivery choice

Status: ready, part of the reviewed Technical Design.
The reviewed [Specification](../spec.md) owns the five-second whole-return
bound. This document closes source and delivery custody;
[Technical Design](design.md) owns runtime and diagnostic flows.

## Selected source

Use a local path patch of **only sqlx-core 0.9.0**, copied from the exact
published crate archive at
`https://static.crates.io/crates/sqlx-core/sqlx-core-0.9.0.crate`, whose SHA256
is `05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb`.
Place its published package contents in `vendor/sqlx-core/`. Keep upstream
license files, normalized standalone Cargo.toml, source and provenance.
Do not copy or modify the shared Cargo registry checkout. A local Git fork,
remote fork, branch dependency or PR-head pin is not selected.

Apply only the five-second whole-return timeout equivalent to current
upstream PR #4350/#4407 around the owned `Floating::return_to_pool` future.
The source adjustment is confined to
`vendor/sqlx-core/src/pool/connection.rs`; no driver transaction, TLS,
migration, feature or version change is part of this backport. Retain native
minimum-connection maintenance after the bounded return. The existing
five-second close-on-drop behavior remains. Actual adapter tests stay in
the repository's existing test owner, not a duplicated upstream test suite.

Record archive URL/checksum, upstream PR and exact compared head, exact
local patch, touched-source hashes, ownership and retirement condition in
`vendor/sqlx-core/PATCHES.md`. That small record distinguishes template-owned
delta from the otherwise unmodified upstream payload. The copied crate is
not a workspace member; it is a dependency with its own upstream MSRV and
lints. Current Rust 1.98 remains sufficient.

Cargo.toml gets one profile-marked `[patch.crates-io]` entry:
`sqlx-core = { path = "vendor/sqlx-core" }`. The root workspace explicitly
excludes that directory so Cargo and cargo-chef never treat it as application
source or generate an empty stand-in for it. Restructure the existing exclude
array so its current gRPC entry and the PostgreSQL vendor entry each have
their own optional-profile marker; do not make one depend on the other.

## Intentional source-only locked resolution

Cargo.lock must resolve a single sqlx-core 0.9.0 from the local path, with
its version and dependency list unchanged; all remaining SQLx packages stay at
the current registry release. The root user-authorized source backport
explicitly includes this lock change. Every Cargo invocation remains --locked.
There is no cargo update/generate-lockfile exception or registry-cache edit.

Interpretation of rust-dependencies: its exact instruction is “Never edit the
lockfile by hand or bypass integrity checks to fix a download.” This change
is a checksum-verified intentional dependency source replacement, not a
download repair. The responsible dependency executor owns one mechanical,
fail-closed lock projection, using the repository initializer's existing
`tomllib` plus `[[package]]` record-preservation pattern. No general new lockfile
updater is introduced.

The bounded procedure is:

1. Before extraction, hash the downloaded published archive and require the
   exact expected checksum above. Reject mismatches. Compare the unpacked
   manifest, feature/dependency declarations and pristine source with that
   verified archive before applying the isolated return patch.
2. Parse the current root Cargo.lock without rewriting it wholesale. Require
   exactly one package record with name sqlx-core, version 0.9.0, source
   `registry+https://github.com/rust-lang/crates.io-index` and the expected
   checksum. Also require no existing path duplicate, no unused patch record
   and the intended root Cargo path-patch declaration/workspace exclusion.
3. Project only that package record's source identity: remove its source and
   checksum fields because a Cargo path package has neither. Preserve its
   name, version, dependency vector/order and every other package byte. Parse
   the result again and assert that the two removed fields are the entire
   semantic difference. Record before/after lock hashes and the expected
   source transition in the implementation evidence. This is a scripted
   one-time transformation of an authorized artifact, not hand editing.
4. Run cargo metadata --locked --offline and the matching build --locked.
   Verify one sqlx-core 0.9.0 resolves to the exact vendor manifest as a
   non-workspace dependency, with no new package/version/features or unused
   patch warning. Compare the resolved graph with the baseline after
   normalizing only that source identifier. Cargo acceptance of the locked
   graph, not the script's intended output, is decisive.
5. If Cargo requires any additional resolution, do not remove --locked or
   relax an integrity check. Return the actual graph difference to Technical
   Design/dependency ownership. A source-only backport must not silently
   become a version/feature upgrade. Unchanged-graph success closes the input;
   no permission question is needed for this routine local authorized change.

The authoritative local precedent is
`scripts/lib/template_init.py::_lock_records`, `_LockRecord`,
`_replace_lock_dependencies` and `_project_cargo_lock`: they parse exact record
identities, preserve unrelated bytes, fail on source-shape drift and validate
projected output with locked offline Cargo metadata. Reuse that pattern; do
not alter the initializer merely to perform this one source transition.

## Portable image path

The current .dockerignore excludes every top-level path except an allowlist;
vendor is not currently admitted. Add a PostgreSQL-profile marker allowing
`vendor/` in the Docker context. `COPY . .` then supplies the real package to
the planner and final source stages.

The cooked stage currently copies recipe.json alone. Add a PostgreSQL-marked
`COPY --from=planner /src/vendor/sqlx-core /src/vendor/sqlx-core` before the
first cargo-chef cook. This supplies the real excluded dependency to every
cook (service, migrator and worker) and makes its content part of the
dependency-layer cache key. Do not depend on a host Cargo cache, network patch
application or a file edited after the cook. Retained binaries continue using
cargo auditable and the existing image/scan/lifecycle gates.

The resolved cargo-chef 0.1.78 source was inspected: `skeleton/read.rs:40–54`
enumerates workspace members for skeleton manifests; `skeleton/mod.rs:151–194`
writes dummy target files for those manifests. Explicit workspace exclusion
plus the real-source copy keeps sqlx-core out of that dummy set while making
the path patch resolvable in the cooked image. This is source-level feasibility,
not a claim that an image with the patch was built.

## Initialization, projection and validation ownership

`scripts/lib/template_profiles.json` owns removal of `vendor/sqlx-core/`
when `database = none` and the new markers for the Cargo patch, workspace
exclude entry, .dockerignore allowlist and cooked-stage copy. Retained
PostgreSQL initializations keep them together. The existing projection's
dependency reachability prunes the now-unreachable sqlx-core lock record when
PostgreSQL is absent; it does not assume every source-less dependency is an
application package. No special lockfile rewrite for the vendored crate is
needed from the inspected `_project_cargo_lock` implementation.

The new vendor family joins `scripts/ci/changed-surfaces.sh` explicitly:
dependency, Rust, PostgreSQL integration and runtime-image surfaces, plus
source-template initializer/runtime selection when applicable. A vendor-only
patch must not receive only the generic `*.rs` classification and silently
miss integration/security/image proof. The existing affected-crates router
already falls back for Rust outside crates/; keep that truthful workspace
route rather than pretending vendor code is an application crate. Extend the
classifier self-test only for this new classification contract.

Prove representative retained and absent PostgreSQL projection/locked metadata
without multiplying database/profile matrices. Existing initializer/image
gates retain their owners and scope; no new CI job, tool, runtime image or
publication workflow is required. SQLx CLI remains 0.9.0 because query metadata
format and migration APIs did not change. `make deny` still assesses the
locked version/licenses; source provenance and the isolated delta need their
separate custody record because the path package has no registry checksum in
Cargo.lock. Do not add a new advisory-ignore or Git-source allowance.

## Exact owner map for the source branch

| File/family | Present responsibility |
| --- | --- |
| vendor/sqlx-core/ | Published third-party payload; only pool/connection.rs is the runtime backport, PATCHES.md is its local custody record |
| Cargo.toml; Cargo.lock | Single patched dependency resolution and workspace exclusion, with all other SQLx components unchanged |
| .dockerignore; build/docker/Dockerfile | Vendor content enters planner, cooked dependency layer and final sources; absent PostgreSQL removes those instructions |
| scripts/lib/template_profiles.json | PostgreSQL selection contains/removes the vendor and its structural references together |
| scripts/ci/changed-surfaces.sh and its existing self-test | Vendor-only changes select the existing matching gates |
| docs/architecture/persistence.md; docs/validation/postgres.md | Active behavior, bounded cleanup tradeoff, upstream retirement and proving scope |
| test/tests/postgres.rs and existing support | Permanent real-server pool/finality/regression observations, using current harness |

Acquisition events remain ordinary adapter operation-boundary work; their
replacement design must identify exact callers and prevent double counting,
but they require no new pool type or Executor implementation. Sizing and
readiness guidance remain their existing owners.

## Total-cost comparison and retirement

This path adds a 110-file/approximately 649 kB third-party payload plus a
small custody note and changes six existing source/delivery control files
(Cargo.toml, Cargo.lock, .dockerignore, Dockerfile, profile inventory and
classifier). That is materially more than eleven changed Rust lines. It
incurs backport review, provenance and one future removal change. It also
preserves existing runtime pool mechanics, all provider/public SQLx types,
query metadata, session admission, password refresh, retirement and shutdown;
no application cleanup task, custom pool Executor, or borrower-state
abstraction is added.

Generic Deadpool avoids vendoring and those delivery changes, but permanently
moves connection Manager policy, rotation, exact idle timestamps, periodic
retirement and close-completion accounting into the template and changes
shared-pool consumer/fixture types. Explicit acquisition removes the proposed
Executor facade cost, but not these lifecycle obligations. The selected
temporary library backport is therefore smaller in lasting runtime
responsibility for this existing service template, even though it is larger
in checked-in source bytes. This is the actual tradeoff; neither source byte
count nor the patch's line count alone settles it.

Retirement condition: a published, otherwise acceptable SQLx release includes
an equivalent whole-return bound and passes the affected cancellation/reuse/
finality proof. Upgrade through the dependency owner, remove the Cargo patch,
vendor payload and now-unused profile/Docker/classifier exceptions, and retain
the behavioral regression. A changed SQLx version without proven equivalent
return ownership reopens the decision; do not silently let Cargo stop using
the backport. No ETA, upstream merge or future release is promised.
