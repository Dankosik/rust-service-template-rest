# CI/CD Production Readiness

What the pipeline proves about a candidate, in the order a change meets it,
and the decisions behind each gate with the alternatives they beat
([recorded at the end](#decisions-recorded-here)).

## Local leaves

Ordinary local completion is a matching build and relevant tests under
[AGENTS.md](../AGENTS.md#validation-budget); [Validation Routing](validation-routing.md)
selects anything beyond that. `make verify` runs the surface-aware route's
local steps and records a receipt, leaving the heavy steps and the initializer
matrix to CI; `ALLOW_FULL=1 make check` is the explicit full gate. Local
completion does not assert platform admission, runtime behavior, or a
release; an explicit request for green CI or a release retains that outcome
until the real result exists.

## Pull-request and push CI

[ci.yml](../.github/workflows/ci.yml) classifies the exact diff once
(`scripts/ci/changed-surfaces.sh`, the classifier `make verify` uses; a pull
request that changes the classifier is judged by both the base and the head
version) and starts only the jobs its surfaces select. A pull request runs the
gate, every job below except `image`, `runtime-progress`, `jobs-reliability`
and the `initializer` parts; those run after merge, on `main`, with CodeQL's
Rust analysis ([Red main](#red-main)):

| Job | Selected by | Proves |
| --- | --- | --- |
| `quality` | Rust source, manifests, lint config, OpenAPI, instructions, duplication, architecture | format; on pull requests clippy and tests of affected crates and dependents, on `main` and manifest changes the workspace; cargo-shear; Redocly and contract drift; oasdiff against the base; skills; clone admission and declared crate directions; checker self-test (over `make lint`'s artifacts) and source-only checker projections |
| `quality-profiles` | Rust source, manifests | the native transport regressions (manifests) and the optional profiling configurations, over `quality`'s restored cache |
| `quality-validation` | validation system | the classifier, affected-crate planner, validation-lock and `make verify` self-tests |
| `security` | manifests, `deny.toml`, workflows; tool manifest and image on pull requests | cargo-deny (advisories, licenses, bans, sources); Dependency Review, fail on high, pull requests only; zizmor with the online audits |
| `secrets` | every event except a schedule without a policy change | Gitleaks over the commits since the base; complete `HEAD` ancestry on tags, manual runs, selected schedules, and a push without a readable base |
| `delivery` | shell, workflows, tool manifest, image, publication metadata | actionlint; ShellCheck over the changed scripts; `tools-check`; BuildKit Dockerfile checks; the publication metadata self-test |
| `image` | Docker/image sources, the root `Cargo.toml` (release profile and workspace dependencies), and any selected profile image path | one local-default image: build, hardened lifecycle asserting `app.commit`, and Trivy for image changes; retained profile details below |
| `docs` | Markdown, documentation, image-input policy/checker and Docker context controls | Railway watch forms agree and cover admitted image inputs; every relative link and `#fragment` resolves (lychee, offline, pinned container); no Rust toolchain |
<!-- template:begin grpc:docs-ci-grpc-gates -->
| `grpc` | schema, generated contracts, generator, Buf configuration | Buf format/lint, repeat deterministic generation, committed drift and FILE compatibility against the actual PR base |
<!-- template:end grpc:docs-ci-grpc-gates -->
| `required` | always | fails when any job failed or was cancelled; requires terminal success for a selected initializer matrix |

The `quality-profiles` job runs the selected native SQLx, NATS, Smithy and Hyper
regressions serially. The native helper records each package's source,
manifest, lock, normal/build graph, selected test names and nonzero count
before execution, then uses its locked standalone manifest and the production
feature selection. This is separate from the workspace lock used by adapter
and integration proof; a removed profile removes its optional native source and selected entry.
Hyper remains unconditional because telemetry retains the client.

<!-- template:begin postgres:docs-ci-postgres-gates -->
With PostgreSQL retained, `quality` also checks migration history, lints the
added migrations with Squawk (`SQUAWK_CLI_VERSION`, through `npx` as Redocly
is), and checks the embedded source rules. `integration` runs the real database proof on provider, migrator,
migration, DB-test, Compose or DB-script changes, and then `make sqlx-check`:
`.sqlx/` must equal what the checked statements produce against a database
holding exactly the embedded migrations. `sqlx-cli` has no release binaries, so
the job builds the pinned version once and caches it by version. A migration change selects the image
rehearsal in place of plain lifecycle: `/migrate` against a fresh database,
`no_change` replay, then lifecycle with the pool open.

The `postgres_maintenance_rules` surface selects the dedicated
`postgres-maintenance-rules` job and the same `make postgres-maintenance-rules`
command in `make verify`. It covers the delivered rule file, deterministic
fixtures, runner, tool pins and shared routing owners. `required` demands
success when that surface is selected. The runner extracts only `promtool`
from [Prometheus 3.5.0's official release](https://github.com/prometheus/prometheus/releases/tag/v3.5.0),
checks the archive against the release SHA256 pinned in `tools/versions.env`
on every use, and keeps its cache in this checkout's
`target/postgres-maintenance-tools`. No Prometheus server or global install is
needed. The sustained laboratory is an explicitly invoked measurement, never
an ordinary CI gate.
<!-- template:end postgres:docs-ci-postgres-gates -->

<!-- template:begin messaging:docs-ci-messaging-gates -->
With JetStream retained, `messaging_integration` selects one real-NATS suite
and the actual Go compatibility bridge when adapter, profile, Compose, or
bridge inputs change. It has a messaging-only representative with no PostgreSQL
or jobs and a combined representative only when another retained profile needs
it. CI also checks locked offline Cargo metadata after initialization, the
resolved NATS image digest, and dependency policy. These are selected surfaces,
not a Cartesian multiplication of every profile, database, and harness.
<!-- template:end messaging:docs-ci-messaging-gates -->
<!-- template:begin cache:docs-ci-cache-gates -->
With the cache profile retained, the `cache_integration` surface runs
`make test-integration-cache` against Compose Valkey in the integration job.
<!-- template:end cache:docs-ci-cache-gates -->
<!-- template:begin object-storage:docs-ci-object-storage-gates -->
With the object storage profile retained, the `object_storage_integration`
surface runs `make test-integration-object-storage` against Compose versitygw
in the integration job. No CI job writes to a live provider bucket.
<!-- template:end object-storage:docs-ci-object-storage-gates -->
<!-- template:begin outbox:docs-ci-outbox-gates -->
With the outbox profile retained, the database-and-NATS integration selection
covers transactional commit/rollback, live-key same/conflict/lost outcomes,
outage snooze/recovery, uncertain completion, durable consumer effect dedupe,
and publication despite occupied webhook slots. The initializer retains
meaningful outbox-only and combined representatives and verifies the initialized
locked graph with `cargo metadata --locked --offline`. These checks join the
assembled candidate's final CI route; a documentation change does not create a
separate acceptance gate.
<!-- template:end outbox:docs-ci-outbox-gates -->

The source template additionally selects the initializer matrix on
`initializer_runtime`, the paths that can change an initialized service's
build and tests or the initializer itself. Parallel parts together run
`make template-init-check`, and `scripts/ci/initializer-matrix.py` plans them
from the same diff: a changed crate, migration, test, or proto path selects
only the runtime graphs whose profile tuple keeps it (the initializer's own
removal inventory decides), and any other initializer path selects all. The
Cargo-free `initializer (projections)` job checks the canonical profile/harness
projections on every `module_initializer` change. The runtime inventory
`scripts/ci/template-init-check.sh`,
planner `scripts/ci/initializer-matrix.py` and
projection inventory `scripts/tests/template-profile-projections.py` own the
exact tuples, part assignments and commands, rather than hand-maintained totals
in this guide. Baseline full checks and focused all-target/profile checks retain
their scopes; representative capability additions do not repeat full database
suites across every neighboring profile or harness. Every runtime graph also
runs the initialized service's `make unused-deps`.

The selected artifact tuples also build each retained executable with the
Dockerfile's locked release/package/binary/default-feature selection and check
its image entrypoints and Rust inventories, one `image-derived` job per leg of
tuples that `scripts/ci/initializer-matrix.py` groups within a cache family.
They reuse a tuple's artifact and add no database scenario. Debug/workspace or
all-target checks alone cannot establish those selected release graphs. Each
part reuses one absolute Cargo target and records its selected tuple, command,
result and duration.

In the source template the database surface also selects the
`jobs-reliability` job, `make jobs-reliability-reference`: the exact-commit
jobs recovery and initialized-service upgrade reference on its own Compose
PostgreSQL and NATS ([test/README.md](../test/README.md)), beside
`integration` rather than after its suites.

<!-- template:begin outbound-auth:docs-ci-outbound-auth-gates -->
OAuth representatives cover standalone, authenticated, messaging and maximal
PostgreSQL neighbors. Retained test targets compile once per graph; database-free
graphs do not request the removed integration-test feature. The workspace quality
gate runs the OAuth behavior suite. The `oauth_integration` surface runs
`make test-integration-oauth` in its own `oauth-integration` job against the
script's digest-pinned throwaway Keycloak, independently of Compose integration.
<!-- template:end outbound-auth:docs-ci-outbound-auth-gates -->

A change to projected text alone selects `module_initializer` without the
runtime surface and runs only the `initializer (projections)` job.
Other quality, security, image and database jobs keep their own gates. The
source-only runner and Make include are removed from derived services, so
initializer proof cannot recur there. [Initialization validation](template-sync.md#validation-boundary)
defines the proof scopes and focused mode.

A weekly schedule and manual dispatch select every surface, because advisory
databases move without a commit. Tags select every surface too. A docs-only
pull request runs `changes`, `docs`, `secrets`, and `required` and no Rust job,
plus `initializer (projections)` when it touches projected documentation
(historical upstream evidence: [#11](https://github.com/Dankosik/rust-service-template-rest/pull/11)
before the `docs` job existed; it adds a link check, not a toolchain).
Every action is pinned by commit SHA with its version beside it; tool
versions come from `tools/versions.env` through `GITHUB_ENV` and
`taiki-e/install-action`.

A pull request or merge-group run sets `post_merge=false`, and `required`
accepts the post-merge jobs as skipped; a push, tag, schedule or manual run
selects them by surface as before. A push to `main` replaces the run still
pending in its concurrency group, so one run can prove several merges; its
`changes` job therefore classifies the diff from the `green` branch, the last
commit `ci` and CodeQL both proved, and falls back to the previous push when
`green` is missing or not an ancestor. The `initializer` and `image-derived`
matrices run four and two at a time, so a `main` run leaves about six of the
account's twenty runners to pull-request gates.

Cargo caches go through `Swatinem/rust-cache`: keyed by the toolchain, every
manifest and lockfile, and the `CARGO_*`/`RUST*` environment, and saved only by
a push to `main` after a successful job. Every job that uses them installs the
pinned toolchain through [`.github/actions/rust-toolchain`](../.github/actions/rust-toolchain/action.yml),
which removes every other one: the action hashes all installed toolchains, so
the runner image's moving `stable` would otherwise change every key. The
initializer parts share one cache per family, which only the family's saving
part writes. The saved target keeps dependency
artifacts alone; workspace crates and test binaries rebuild on every checkout
anyway, and keeping them pushed the repository past its 10 GB cache budget, so
caches evicted one another and most pull requests started cold. For the same
budget the dependencies build without debuginfo (workspace crates keep line
tables) and the archives are written at zstd level 12. The first save owns a
key until the lockfile changes, so a save must hold everything its job can
build: `quality` saves only after a workspace build, and `integration`
compiles every retained suite before it saves.

[codeql.yml](../.github/workflows/codeql.yml) runs CodeQL for Rust
(`build-mode: none`) after merge when Rust source or manifests change, or the
analysis itself, and for Actions on pull requests too when workflows change,
with `security-events: write` scoped to the analyze jobs. On `main` it
classifies the diff from `green`, as `ci` does. The Rust analysis covers shipped code: its
[configuration](../.github/codeql/rust.yml) ignores the test crate, tools, test
directories and vendored examples and benches, the `test` cfg is turned off,
and the extractor's consistency and telemetry diagnostics are excluded; the
vendored sources stay, because CodeQL's SQLx models reach the workspace's
queries through them. It points the extractor's `cargo_target_dir` at a
persistent target restored with the crate registry; like the CI caches, only a
push to `main` writes it, after a successful analysis. `codeql-required`
accepts skipped analyses and rejects failed ones.

GitHub Rulesets or organization policy own merge admission: require
`required` and `codeql-required`. The repository does not rewrite its own
protection settings. Dependency Review needs the repository dependency graph
(Dependabot alerts enabled), an operator step for a derived repository.

## Readability gates

The existing `quality` job and `make verify` invoke the same
`duplication-check`, `architecture-check` and `quality-check-self-test` targets.
The `duplication` classifier surface follows handwritten main-workspace Rust,
`.jscpd.json`, its reviewed baseline and checker. `architecture` follows Cargo
declarations, its policy/checker and component-boundary documentation. Manifests,
lockfiles, toolchain, tool pins and shared integration select both. Their
self-test requires the pinned toolchain and `npx`; a missing prerequisite fails.
The existing `required` job consumes `quality` with unchanged failure semantics.

jscpd 5.4.0, pinned in `tools/versions.env`, discovers exact substantial Rust
clones. The Python wrapper validates its JSON coordinates and admits only the
bounded service-owned cases in `quality/duplication-baseline.json`; dedicated
test files produce a separate report-only scan. The report command never
refreshes policy. The architecture checker applies
`quality/architecture.json` to Cargo metadata declarations, including build,
optional, alias and inactive-target edges. Clippy owns the nesting limit of six.
See [maintenance and setup](build-test-and-development-commands.md#readability-and-crate-boundaries).

In the source template `template-quality-projections` exercises both checker
CLIs against four renamed profile graphs: minimal, all retained capabilities,
and inbound-only/outbound-only webhook profiles. The two directional profiles
also compile the retained webhook tests against the shared test recorder. It runs once per graph, separately from harness equality and runtime
builds. Ordinary projected-documentation changes retain the Cargo-free
projection job. Generic checkers and their self-test are portable sync owners;
config, architecture classifications and clone admissions remain required local
authorities and are never overwritten by sync.

## Generated contracts

The OpenAPI document is generated from the handlers and committed; the drift
test in `make test` refuses a stale copy, Redocly lints it, and pull requests
compare it with the base through oasdiff. Generated output is never edited by
hand ([Generated Contracts](validation/generated.md)).

## Secrets and dependencies

`.gitleaks.toml` carries the default rules and documented fixture/build-output
allowlists. `.gitleaksignore` excludes four exact commit-qualified findings
for public historical NATS and Smithy fixtures, with provenance and SHA256
hashes beside each group. Initialized services retain it because they inherit
those commits; the exclusions do not cover new commits or worktree copies.
There is no baseline report. `deny.toml` restricts the
graph to the two Linux gnu targets and lists the one advisory ignore with its
reopen condition. Duplicate crate versions are warnings, not failures.
Dependabot updates Cargo dependencies, GitHub Actions in workflows and in the
composite publication action, and the Dockerfile `FROM` digests; the tool
manifest is bumped by hand.

Candidate history admission scans `HEAD` and every reachable ancestor,
including merged-parent ancestry, with native `--log-opts=HEAD`. The target
refuses shallow or unavailable repository/`HEAD` state with exit 2 before
scanning; CI keeps `fetch-depth: 0`. Unrelated fetched refs do not become inputs
to immutable candidate admission. The separate explicit native `--all`
repository audit uses the same rules, ignores and redaction
([Security Validation](validation/security.md)).

## Publication

[cd.yml](../.github/workflows/cd.yml) has one job, gated by the repository
variable `ENABLE_GHCR_PUBLISH == 'true'`. Main publication consumes a
successful same-repository push run of `ci`, waits for the exact-SHA CodeQL
run, and checks out that SHA; a `v*` tag waits for its own `ci` and CodeQL
runs. The shared [publish-image action](../.github/actions/publish-image/action.yml):

1. names the candidate (`scripts/ci/publish-image-metadata.sh`); a release
   tag must equal `v<crate version>`;
2. builds one run-scoped candidate for the exact commit with the same make
   target CI uses;
3. repeats the hardened lifecycle check and the Trivy scan;
4. writes a CycloneDX SBOM from the pinned Trivy container;
5. pushes the candidate and resolves its digest;
6. signs the digest keyless with cosign and attests provenance and SBOM;
7. verifies the signature and both attestations back out of GHCR;
8. records the digest, uploads the SBOM artifact;
9. promotes `sha-<12>` + `main` or `v*` + `latest`, reading each tag's digest
   back; a partial failure records the promoted and failed tags.

Public tags never move before verification. Consumers verify with
`cosign verify --certificate-identity
https://github.com/<owner>/<repo>/.github/workflows/cd.yml@refs/heads/main`
(or `@refs/tags/v…`) and `gh attestation verify oci://<image>@<digest> --repo
<owner>/<repo>`. Nothing is published by this template repository; stage 12
exercises the path once.

## Red main

[main-health.yml](../.github/workflows/main-health.yml) runs when `ci` or
CodeQL completes a push run on `main`. It reads the latest decided push run of
each, re-runs a first failed attempt once to separate a flake, and on a second
failure opens, or comments on, the one open issue labeled `main-red` with the
failed jobs and the commits since `green`. When both workflows succeeded at the
same commit, `green` fast-forwards there and the issue closes. CD publishes only
after a successful `ci` push run, so a red `main` ships no image; template sync
reads `green` ([Adopt a committed source](template-sync.md#adopt-a-committed-source)).

Repair while the issue is open:

1. Name the failing commit. With several commits since `green`, push a branch
   at a suspect commit and dispatch `ci` on it (`gh workflow run ci.yml --ref
   <branch>`), which selects every surface; delete the branch afterwards.
2. Fix forward: a pull request that makes the failed jobs pass, through the
   same gate as any change. Reproduce a post-merge job with its `make` target
   from the job's steps.
3. When `main` is still red two hours after the issue opened, or two fix pull
   requests failed, revert the failing commit through a pull request instead.
   A revert of a migration that shipped in a `green` image is not a revert;
   fix forward with a new migration.

Other pull requests keep merging while `main` is red; the failed jobs of the
next `main` run name any new breakage beside the open one.

## Recovery

- Failed CI changes no external state; each job's containers are removed by
  the scripts' traps.
- A failed publication never promotes public tags; the run-scoped candidate
  tag is the only pushed reference.
- Retain and verify the accepted immutable image digest for rollback, together
  with compatible configuration. A mutable tag or rebuilt old commit is not that
  retained artifact. Source deployment has its own [deployment custody](railway-deployment-profile.md#rollback).
- Startup identity and readiness do not prove old SQL, data or payload compatibility;
  the service owns its mixed-version window and [recovery admission](production-contract.md#operation-and-recovery).

## Decisions Recorded Here

Made in stages 4 and 5 with the research behind them (versions as read on
2026-09-18; claims marked *verified* were executed on this workspace). A
later change reopens one only with new evidence.

### Tool manifest

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| `tools/versions.env`, `NAME=value` lines read by `make` (`include`), shell (`.`), and CI (`>> $GITHUB_ENV` after stripping comments) | `mise` with `mise.lock`; `cargo-run-bin`; a `tools/Cargo.toml` of tools as dependencies; `cargo-binstall` as a prerequisite | Cargo has no project-local tool table or checksum-verified binary distribution; a versions file needs no parser and no new prerequisite. `mise` reopens if the tool set outgrows Cargo + Go + Node + Docker |
| Cargo tools built once per version with `cargo install --locked --root <git-common-dir>/tools/<crate>-<version>` as Make prerequisites; `taiki-e/install-action` (v2.87.14) in CI with `CI=true` resolving them from `PATH` | compiling in CI | the four tools cost about six minutes to compile (*verified*: cargo-shear 58 s, cargo-deny 121 s, zizmor 161 s), acceptable once per workstation, not per run |
| `cargo-chef` and `cargo-auditable` as Dockerfile `ARG` defaults, asserted equal to the manifest by `make tools-check` | passing them as build arguments only | Railway passes no build arguments |
| Redocly through `npx`, oasdiff, Gitleaks, and actionlint through `go run <module>@v<version>` | binaries with checksum scripts | `npx` and the Go module checksum database already verify what they run; Go is a prerequisite for oasdiff anyway (`go run` of Gitleaks: 16 s cold, 1 s warm, *verified*) |
| ShellCheck, Trivy, and lychee as digest-pinned containers | native binaries | Haskell and Go binaries with no Cargo or Go module path; Docker is a prerequisite for image work |
| Base images pinned once in the Dockerfile `FROM` lines, moved by Dependabot's `docker` ecosystem; the manifest does not repeat them | a manifest copy | one owner per pin; `make tools-check` asserts every `FROM` carries a digest and the rust tag equals the toolchain channel (*verified* that a drifted `ARG` default fails) |
| The manifest is not Dependabot-managed | Renovate regex manager | deliberate bumps; a derived repository may add Renovate |

### Gates

| Gate | Decision | Rejected | Why |
| --- | --- | --- | --- |
| Advisories, licenses, bans, sources | `cargo-deny` 0.20.2; `[graph] targets` = the two Linux gnu triples; `allow-wildcard-paths = true`; `multiple-versions = "warn"`; no advisory ignore (the `paste` one closed with `utoipa-axum` 0.3.0) | `cargo-audit` (same database, no reachability analysis exists for Rust); `cargo-vet` (a template cannot ship audits) | without `targets`, macOS-only crates enter license decisions about the Linux binary (*verified*); path dependencies count as wildcards otherwise (*verified*); `base64`, `getrandom`, `hashbrown`, `syn`, `tower-http` resolve twice through upstream crates and are warnings |
| Unused dependencies | `cargo-shear` 1.13.4, on the workspace and on every initializer runtime graph | `cargo-machete` (two false positives here: `humantime-serde` used through `#[serde(with)]`, `vergen-gitcl` in `build.rs`); `cargo-udeps` (nightly) | *verified*: cargo-shear found the unused `http` and `hyper` in `infra-http` with no false positive. The full workspace uses every declaration, so only an initialized graph shows one that a profile left without users: 55 of the 68 graphs failed `make unused-deps` before the initializer dropped those declarations (*verified*, 2026-10-20) |
| Dependency metadata in the binary | `cargo auditable build` | plain `cargo build` | Trivy reports 178 Rust packages from the image's `/service` (`rustbinary`); without it the scan sees only Debian packages (*verified*) |
| New vulnerable dependencies | `actions/dependency-review-action` v5, `fail-on-severity: high`, pull requests only | — | needs the repository dependency graph (Dependabot alerts enabled), which was off here and had to be enabled (*verified* by the first failing run) |
| Secrets | Gitleaks 8.30.1 with range or complete candidate-ancestry admission; default rules and documented allowlists; four commit-qualified public-fixture fingerprints in `.gitleaksignore` | unrelated refs as candidate admission inputs; a baseline report or unqualified historical-fixture exclusions | native `HEAD` retains reachable ancestry; exact fingerprints cover only the reviewed historical findings; `gitleaks dir` ignores `.gitignore` and read 153 MB of `target/` without the allowlist (*verified*) |
| Workflow syntax and security | actionlint 1.7.12 with host integrations off; zizmor 1.30.1 regular persona with `GH_TOKEN` for the online audits | — | zizmor found `cache-poisoning` on the original single-job workflow, fixed by restore-always/save-on-main (*verified*); its `dangerous-triggers` and `self-repository` findings on `cd.yml` are ignored inline with reasons (the job's `if` guards, and actionlint does not yet parse GitHub's `$/` form) |
| Dockerfile lint | `docker buildx build --check` | hadolint | BuildKit's linter is built in |
| Static analysis | CodeQL for Rust (GA since CodeQL 2.23.3) and Actions, `build-mode: none`, advanced setup with `codeql-required`; Rust after merge (2026-10-22) | — | default setup was not configured, so the workflow does not conflict |
| Image vulnerabilities and SBOM | Trivy 0.74.0 container: `--severity HIGH,CRITICAL --ignore-unfixed --exit-code 1`; CycloneDX SBOM from the same container over the pushed image | grype/syft (a second scanner); `cargo-cyclonedx` (describes the source graph, not the image); `aquasecurity/trivy-action` (a second Trivy pin) | 194 SBOM components on the scaffold image: 178 `pkg:cargo`, 14 `pkg:deb` (*verified*) |
| Links | lychee 0.24.2 container, `--offline --include-fragments`, over `git ls-files '*.md'` | lychee via `cargo install` (no install-action manifest, compiles reqwest and tokio in CI); `lychee-action` (CI-only, second pin); `mlc` and `markdown-link-check` (fragment checking not documented); a template-owned script (a solved problem) | *verified*: reports `Cannot find fragment` and `File not found`; external URLs excluded on purpose so the gate cannot flake |
| Test runner | `cargo test` | `cargo-nextest` | no present pressure (no test mutates the process environment, every wait is bounded, 80 tests run in seconds); reopens with the stage 8 container-backed tests |

### Routing

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| Ported `changed-surfaces.sh` and `git-changed-paths.sh`, fail-closed, `--union BASE`, self-tests | `dorny/paths-filter`, `tj-actions/changed-files` | no action classifies fail-closed, reports unclassified paths, unions the base classifier, and runs unchanged under `make verify` |
| `affected-crates.sh` over `cargo tree --locked --workspace -i <pkg> -e normal,build,dev` | a hand-maintained dependency map | the dev edge reselects a dependent's tests (*verified*: `health` → `infra-http`, `service`); a manifest, lockfile, or toolchain change and a closure at 80% of the workspace fall back to the workspace because feature unification can change an untouched crate |
| One `changes` job feeding conditional jobs and an always-reported `required`; `quality` installs the toolchain only when a Rust surface is selected | one job with conditional steps | a docs-only pull request runs no Rust job |
| `ALLOW_FULL` guards `make check` only; `ALLOW_HEAVY` guards the image targets, the database proof, the migration rehearsal, and the history scan; `CI=true` satisfies both | guarding `lint` and `test` too | `make build`, `make test`, and `make lint` are the ordinary commands AGENTS.md names |

<!-- template:begin postgres:docs-ci-postgres-routing -->
`db_integration` selects database proof; `migrations` selects static history
with the lint and the image rehearsal in place of the plain lifecycle check. Separate
surfaces let a schema change require image rehearsal without rebuilding for
every adapter-only change.
<!-- template:end postgres:docs-ci-postgres-routing -->

### Pipeline time

Reopened on 2026-09-23 with CI evidence: after stage 9, pull-request runs took
22–36 minutes and pushes to `main` up to 43, almost all of it the initializer
job, while every other job finished within 8 minutes.

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| The initializer's staged OpenAPI build reuses an explicit absolute `CARGO_TARGET_DIR`, and `template-init-check.sh` exports its one cache to every initialization | a private target per initialization | about 22 initializations per run each compiled the locked graph cold, about 50 s apiece on the 4-vCPU runner, while each representative's build took 3–5 s and its tests 7–14 s on the warm cache; staged files are written fresh, so Cargo still rebuilds every workspace crate |
| The initializer runs parts balanced on measured warm seconds per graph, in two cache families (`core` graphs retain no idempotency, jobs, messaging, outbox or webhooks; `full` graphs do); each family's cache is written only by its saving part, and only after that part ran all of its graphs (2026-10-20); six `full` and two `core` parts since 2026-10-21 (below) | eight parts with a cache each (to 2026-10-20); twelve parts with a cache each; one sequential job or full database proof for every cross-profile combination | the eight parts took 984–1,472 s on main run 39419885702 (23 graphs in `baseline`, near the 30-minute timeout) and their caches held 6.1 GB plus stale copies against the 10 GB budget; the 68 warm graphs total about 6,400 s, so twelve parts landed near ten minutes each; two family caches hold about 2.9 GB, and a restoring part pays about 30 s per heavy variant its family cache lacks. `required` reads one aggregate result; command-duration receipts expose changes to the critical path |
| Each graph clone dates the vendored `[patch]` crates to the attempt's start (2026-10-20) | the clone's own timestamps | the vendored crates are path dependencies, fresh by timestamp: every clone made them and each dependent (axum, reqwest, tonic, sqlx, aws-sdk-s3, …) stale, so the lint, test and database steps of every graph rebuilt them. Their bytes are identical in every clone of an attempt and no profile renders them; rust-cache never stores their artifacts, and an artifact older than the attempt still rebuilds |
| `initializer_runtime` selects the matrix; projected text alone runs the Cargo-free projections | the whole matrix for every projected path | 250 of the 355 tracked files `module_initializer` selects are documentation, instructions, harness carriers or metadata, which cannot change a build; the projections prove their markers and harness independence in about a minute instead of the matrix's three |
| Workspace crates build with line tables (`[profile.dev] debug = "line-tables-only"` in `Cargo.toml`, which every target cache key hashes) | full debuginfo | the `quality` and initializer caches were 4.2 and 4.1 GB, 8.7 of the repository's 10 GB, so the integration, Go-tool and buildx caches were evicted and one restore took 50–126 s; line tables keep file:line in test backtraces |
| `make verify` leaves heavy steps and the initializer matrix to CI and records a partial receipt | refusing to run without `ALLOW_HEAVY=1` or `ALLOW_FULL=1` | the refusal led agents to run the full matrix on a workstation, 40 minutes and more with several GB of temporary targets, while CI runs the same gates in parallel |
| `REPOSITORY_CI ?= none` in `make/service.mk` makes `make verify` own every step: it admits the heavy guards for its commands, checks Rust as a push does, adds the range secret scan, profiling configurations, OpenAPI compatibility against the merge base, the generator's dependency policy, Go wire compatibility, the jobs reliability reference and the integration compile, names CodeQL, Dependency Review, publication, the scheduled history scan and messaging recovery with their residual, and records `ci_mode: none` with a complete status (2026-10-17) | leaving `REPOSITORY_CI` implicit, so a repository without CI keeps receiving `partially_verified` receipts that name an owner that never runs; running CodeQL locally (its CLI is not licensed for private repositories without GitHub Advanced Security) | a service that deploys from Railway without GitHub Actions needs one local route whose receipt means the same as a green CI run; the steps CI keeps inline stay duplicated in `ci.yml` until CI calls the new targets |
| No registry-only cache restore in `security` and CodeQL | the restores that were there | a cache version includes its path list, so restoring fewer paths than `quality` saves never hit (the CodeQL run of 2026-09-23 reported "Cache not found"); the crate downloads cost seconds |
| Dependencies build without debuginfo (`[profile.dev.package."*"] debug = false` in `Cargo.toml`), and caches are archived at `ZSTD_CLEVEL=12` | line tables for every crate at the default zstd level; zstd level 16 | after the cache pruning the caches still held 11.2 GB against the 10 GB budget (initializer 9.5 GB), so the BuildKit, CodeQL and `quality` caches kept being evicted; without dependency debuginfo and at level 16 the initializer caches measured 5.5 GB (*verified*, 2026-10-01), and a cold part builds faster, while a failing test still reports file:line for workspace frames. Level 16 cost each saving job 2–6 minutes; level 12 costs 35–85 s for archives about 6% larger, paid only by the push to `main` that first saves a key |
| A cache is saved only by a run that built everything its job can build: `quality` after a workspace build, `integration` after compiling every retained suite | saving after any successful job | the first save owns the key. The push that merged the cache change ran only the validation steps and left a 104 MB `quality` cache, so every Rust pull request compiled the workspace cold (360 s against 100 s warm); a push that selected three of the four integration suites left the database proof compiling for 158–194 s against 72 s |
| The canonical projections run in their own Cargo-free job on every `module_initializer` change | a second stage of the `source` part | the projections took 110–150 s after the 150–330 s source suites in one job; side by side the `source` part takes 190 s warm instead of 310 s |
| The four validation self-tests run under `make -j4` | one after another | independent fixtures; 88 s became 48 s |
| CodeQL Rust points the extractor's `cargo_target_dir` at a persistent target, cached with the registry under its own key | the default scratch directory per run | loading the workspace (crate downloads, build scripts, proc-macros, all features on) took about 60 s of a 7-minute analysis; codeql-action 4.38.1 offers neither dependency caching nor overlay analysis for Rust, and the queries (about 3 minutes) and database finalization (about 40 s) are fixed cost on a 4-vCPU runner |

Reopened on 2026-10-20 with CI evidence: pull-request and `main` runs took
32–35 minutes and about 250–270 runner-minutes against the account's twenty
concurrent jobs. `image-derived` (1,873–2,020 s) and `integration`
(1,786–1,935 s) set the critical path, every initializer part took 16–25
minutes, and the caches held 10.6–11.9 GB against the 10 GB budget, so most
Rust jobs started cold.

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| Every Rust job installs the pinned toolchain alone through one composite action | leaving the runner's `stable` installed; `add-rust-environment-hash-key: false` with hand-built keys | `Swatinem/rust-cache` 2.9.2 hashes every installed toolchain: the runner image that added `stable` 1.99.1 beside the pinned 1.99.0 moved every key (`9df4000f` to `75a985ba`), jobs on the old and new images missed each other's caches, and both generations crowded the budget. Hand-built keys would have to repeat the lockfile, manifest and environment hashing the action already owns |
| One `image-derived` job per leg of release representatives, each restoring its initializer family's cache for the staged OpenAPI build; the two `core` representatives share a leg since 2026-10-21 (below) | the serial job | since every build gets its own validation-lock builder, the four representatives shared no BuildKit layer, so the serial job took the sum of their times (1,873–2,020 s) while the longest, graph 65, takes about 780 s alone. Each leg keeps its own receipt and upload; the aggregate is success only when every leg passed |
| CI neither imports nor exports a BuildKit Actions cache for runtime images | exporting the cooked stage on pushes to `main` | the export cost 173 s per push and its blobs never survived the budget (only a 0-byte index remained); the image lane runs for image, manifest and lockfile changes, which change the cooked recipe anyway. Publication keeps its own export |
| The jobs reliability reference runs as its own job (`make jobs-reliability-reference`), and its fixture builds keep the release optimization level without fat LTO | inside the database proof step, with the shipped profile | it added 925–1,100 s after the suite, 735–897 s of it three serial fat-LTO builds; no scenario bound depends on the profile, the receipt records fixture digests without comparing them, and the image jobs build the production profile |
| `quality-profiles` runs the native transport and profiling checks over `quality`'s restored cache without saving it, and `quality-validation` the validation self-tests; the readability checks stay in `quality` | one job | the moved steps ran after lint, build and test: 908 s on a pull request and 1,227 s on `main`. The readability checker self-test reads the compiler artifacts `make lint` reported in the same job, so it cannot move. `required` demands each job its surfaces select |
| The runtime-progress runner builds its specimen and driver in one release invocation | two serial invocations | the two single-threaded fat-LTO links ran one after the other before the measured window; an example and a test resolve the same dev-dependency features, so the specimen is unchanged |
| CodeQL Rust analyzes shipped code only: `.github/codeql/rust.yml` ignores test-only paths and vendored examples and benches, the `test` cfg is off, and the consistency and telemetry diagnostic queries are excluded; the extractor cache key carries the CodeQL version and the vendored manifests | analyzing every tracked Rust file with the default diagnostics; ignoring `vendor/` | the analysis grew from about 7 to 15.5 minutes (928 s on main run 39419885745) after the vendored crates arrived: 706 files, 399 MiB of relations, and test-only code was about half of the lines. Ignoring the vendored sources would drop the SQLx sinks the workspace's queries resolve through; test code ships nowhere, and `#[cfg(not(test))]` items are analyzed now instead. A CLI pins its own extractor toolchain, so a cache built by another CLI version must not restore |

Reopened on 2026-10-21 with CI evidence: the warm all-surface dispatch
39464197796 took 20.8 minutes, while its longest job took 833 s. Its 32
parallel jobs met 18 free runners (CodeQL held two of the account's twenty),
and GitHub gave each freed runner to a queued job in no fixed order, neither
the file's nor the job ids': `quality` started after 467 s and `image-derived
(65)` after 455 s, so the run ended at 1,245 s.

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| The gates that take a few minutes (`quality-profiles`, `quality-validation`, `security`, `delivery`, `docs`, `grpc`, `postgres-maintenance-rules`, `oauth-integration` and `initializer (projections)`) also need `secrets`, the seconds-long gate every event runs, and run under `!cancelled()` | every gate needing only `changes`; one merged job for the short gates | with only the long gates and `secrets` queued at the start, every long gate gets a runner at once, and the short gates fit into the runners that `secrets` and CodeQL's Actions analysis free; a simulation of the run's measured durations under random runner order put the full run at 15.0 minutes against 19.8 for the old layout. A merged short-gate job would still leave 23 jobs for 18 runners. `required` demands each selected gate as before, and a short gate still runs when `secrets` fails |
| Six `full` and two `core` runtime parts of about 14 minutes warm (at most about 19 cold), and one `image-derived` leg for graphs 1 and 7 | twelve parts of about ten minutes and one leg per representative | twelve parts, the source suites and four image legs were 17 long jobs beside `quality`, `image-source`, `runtime-progress`, `integration` and `jobs-reliability`, so no order could start them all; eight parts, the source suites and three legs fill the 18 free runners exactly with those five and `secrets`. In the same simulation one more long job would compete with the short gates for the first freed runners (p90 21 minutes). A part's fixed cost (about 70 s warm and 150 s cold for setup, cache restore and the first initialization) is paid eight times instead of twelve; graphs 1 and 7 take 380 and 489 s on the same cache family, so their leg stays near a part's length |

Reopened on 2026-10-22: agents merge many small pull requests, and the full
proof on each one held every merge for 18–20 minutes and about 220
runner-minutes, then CodeQL's Rust analysis for about 12 more. A merge made
every other open pull request `BEHIND` under the strict up-to-date rule, and
each update reran the whole proof, so parallel work landed about three pull
requests an hour. Of the 70 failed runs among the 200 from 2026-10-05 to
2026-10-22, `quality` failed in 35 (17 alone) and `integration` in 11 (5
alone); of the jobs that now run after merge only `initializer` ever failed
alone, in 3.

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| The gate runs `changes`, `secrets`, `quality`, `quality-profiles`, `quality-validation`, `security`, `delivery`, `docs`, `grpc`, `postgres-maintenance-rules`, `integration`, `oauth-integration` and `initializer (projections)`; `initializer`, `image-source`, `image-derived`, `runtime-progress`, `jobs-reliability` and CodeQL Rust run after merge on `main` | the full proof on every pull request; a draft that defers the heavy jobs until ready for review | the gate is about as long as `quality` (6–8 minutes) and keeps the jobs that caught most defects or guard something a revert cannot undo (a leaked secret, a published contract, a database proof); the moved jobs found a defect alone 3 times in 200 runs. Drafts deferred the same jobs to the end of review, so the wait moved instead of shrinking |
| A red `main` is repaired forward through `main-red`, with a revert after two hours or two failed fixes; merges continue | blocking merges while `main` is red; an automatic revert at the first failure; a merge queue | a merge freeze turns one red job into a stop for every agent; an automatic revert without a culprit search would also revert innocent commits in a batched run (Google's TAP reverts only after a culprit finder and a flake check). GitHub merge queues need an organization-owned repository, which the account declined |
| `main-health` recomputes state from the latest decided runs on each completion and re-runs a first failure once | reacting to the single completed event | a push replaces the pending run, and the health job's own concurrency group replaces its pending event, so event order is not reliable; a state read makes any surviving event sufficient |
| The ruleset requires `required` and `codeql-required` without the up-to-date rule (since 2026-10-22) | strict up-to-date branches | each merge made every other open pull request `BEHIND`, and its update reran the gate; two pull requests that pass apart and fail together now fail the post-merge run on `main` and open `main-red` |
| `changes` classifies a `main` push from `green` | from `github.event.before` | a replaced pending run would leave its commits unclassified: the next run's previous push already contains them |
| `initializer` at most four parts and `image-derived` two legs at a time | unbounded matrices | a `main` run would take all twenty runners and hold every pull-request gate in the queue; the bound keeps about six runners for the gate and makes a full `main` run about 35–40 minutes, which only the red-main signal waits for |

### Runtime image

Historical experiment only: two candidates were built cold from its tracked tree (*verified*,
`--no-cache`, `linux/arm64`, Docker 29.4.0):

| Variant | Runtime base | Cold build | Image | Binary | Trivy (OS) | Trivy (Rust) |
| --- | --- | --- | --- | --- | --- | --- |
| glibc (**selected**) | `gcr.io/distroless/cc-debian13:nonroot` | 150 s (tools 52 s, `chef cook` 62 s, final 10 s) | 43.5 MiB | 7.0 MiB dynamic | 14 packages, 20 unfixed CVEs, 0 fixed HIGH/CRITICAL | 178 packages, 0 CVEs |
| musl | `gcr.io/distroless/static-debian13:nonroot` | 139 s | 9.1 MiB | 6.8 MiB static | 6 packages, 0 CVEs | 178 packages, 0 CVEs |

glibc keeps the target triple the tests run on and leaves the allocator
decision to stage 11 (musl's malloc is the known weak point of
multi-threaded services and the remedy is a custom global allocator, which
that stage owns); the switch is two `--target` lines and the base digest.
Builder and runtime share one Debian release because a trixie-built binary
does not start on `cc-debian12`. `cargo-chef` cooks the dependency layer as
a plain layer because BuildKit does not export `RUN --mount=type=cache` to
`type=gha`; a source-only change rebuilds only the workspace crates, which with
the fat-LTO release profile took 489 s for the service, `/migrate`, and
`/jobs-worker` on a 4-vCPU host (286 s without LTO); see the profile comment in
`Cargo.toml`. Each binary builds in its own stage over the cooked layer, so
BuildKit runs the three single-threaded fat-LTO links in parallel; separate
cargo invocations keep each binary's feature resolution. Publication exports
only the cooked stage to the Actions cache, since every later layer follows
the source copy and no later commit reuses it; CI exports none (Pipeline time).
`rust-toolchain.toml` stays out of the context because rustup would download
`clippy` and `rustfmt` in every stage (*verified*). In that experiment, two
`--no-cache` builds produced byte-identical binaries (`CARGO_INCREMENTAL=0`,
fixed `/src`, `strip = true`, `SOURCE_DATE_EPOCH` for the mtime); their image IDs
differed by layer timestamps. Locked dependency resolution does not guarantee
future binary or image equality. APT indexes and unpinned build packages, the
`docker/dockerfile:1` frontend tag, tool/package download availability and the
security overlay remain mutable inputs. An old checkout is not guaranteed to
remain buildable; rollback custody retains the accepted artifact.
The lifecycle check runs from outside because the base has
no shell or curl. `app.version` stays the Cargo version and the check asserts
`app.commit`; the Go `sha-<12>` version was a workaround for having no module
version. `distroless` tags are mutable, so the digest is pinned and moved by
Dependabot.

<!-- template:begin postgres:docs-ci-migrator-image -->
When PostgreSQL is retained the image also carries `/migrate`, built and cooked in the
same stages as the main binary so the migration job runs the same
image with `--entrypoint /migrate` and needs no migration directory: the
set is embedded at compile time, which is why `migrations/` and the
`test/` manifest enter the build context. `runtime-image-check.sh` accepts
`RUNTIME_IMAGE_NETWORK` and `RUNTIME_IMAGE_POSTGRES_DSN`, so the rehearsal
observes readiness with the pool open (`postgres_pool_opened`) under the
same hardened flags.
<!-- template:end postgres:docs-ci-migrator-image -->
<!-- template:begin worker:docs-ci-jobs-worker-image -->
Jobs or messaging retention also keeps `/jobs-worker`, cooked and built in its
own auditable stage. The image's default entrypoint stays `/service`; the worker
uses `--entrypoint /jobs-worker`. [Container Validation](validation/containers.md)
owns the retained/absent entrypoint and startup-refusal checks.
<!-- template:end worker:docs-ci-jobs-worker-image -->

### Publication and deployment

The composite action rather than a reusable workflow keeps the Fulcio
identity `…/.github/workflows/cd.yml@<ref>`; a release tag must equal
`v<crate version>` because Cargo owns the version. Config as Code
(`railway.toml`) is deprecated with a hard cutoff and closed to new services.

| Decision (2026-10-17) | Alternative rejected | Why |
| --- | --- | --- |
| The template ships `.railway/railway.ts` with `.railway/package.json` and a lockfile pinning `railway` 3.16.0; it names a partial, takes the project and environment from the linked context, and declares the template's variables, preserving the operator-set ones; the initializer writes the service name and repository into it | the document-only snippet (the deferral "`.railway/railway.ts` generation") | the adopting system's task list asks for the file; a pinned SDK and partials make it usable without an owner-supplied project, and the file applies nothing until an operator runs `railway config apply` against a linked project |
| The image-input check reads `.railway/railway.ts` and falls back to the `ts` snippet in a repository's own [profile](railway-deployment-profile.md), naming which it read | requiring the file | repositories initialized before the file existed keep their service-owned document; remove the fallback once the template stops supporting them |
| `IMAGE_TESTS=unit` runs `cargo test --workspace --lib --bins` in a debug stage the runtime stage depends on; the default `none` skips it | `cargo test --release`, which reuses no cooked layer and links every test binary with fat LTO; running the integration suites, which need Docker and a database | a failing unit test stops a platform build before it replaces the running deployment; the debug cook costs a second dependency build once per lockfile |
| The build rules of the open template pull request #274 are ported, not merged: no credential in any build argument, the private-dependency route through a vendored release, and the corrected build-argument sentence | building on #274 | merging it needs the template owner's approval, and its other changes (the Build Speed guide, an `image-results.py` fix) are outside this change; merging #274 later conflicts only in the profile document's build-argument sentence |

The initializer does not create linked Railway inputs or deployment resources.

### Deviations from the Go template

| Go template | Rust template | Why |
| --- | --- | --- |
| `tools/go.mod` + `go tool` | `tools/versions.env`; `cargo install --locked` locally, `install-action` in CI, `go run` for Go tools, containers for the rest | no project-local tool table in Cargo |
| `govulncheck` + `gosec` | `cargo-deny` + CodeQL Rust | no reachability analysis for Rust; `gosec`'s role is CodeQL's |
| `go mod tidy -diff`, `go mod verify` | `cargo-shear`; `--locked` everywhere | no tidy in Cargo; the lockfile is verified by `--locked` |
| static Go binary, `GOTOOLCHAIN=local` | glibc binary on `distroless/cc-debian13`, toolchain file excluded from the context | allocator neutrality until stage 11; target parity with the tests |
| module metadata in Go binaries | `cargo auditable build` | Rust binaries carry nothing by default |
| `APP_VERSION=sha-<12>` | version from Cargo, commit from `VCS_REF` | Cargo owns the version |
| Go build cache as a cache mount | cargo-chef layers | mounts are not exported to the Actions cache |
| `railway.toml` | `.railway/railway.ts` with a pinned SDK | Config as Code deprecated |
| Gitleaks baseline file | documented allowlists and four commit-qualified public-fixture fingerprints | retain complete candidate-ancestry scanning while excluding only the reviewed historical findings |
| `gotestsum` | `cargo test` | no present pressure for a runner |
| no link checker | lychee, offline, with fragments | the stage 5 exit criterion names one |
| `test/README.md` in the documentation graph | deferred to the first `test/` crate | no directory before its first artifact |

### Deferred, with the change that reopens each

- `cargo-nextest`: a measured test-runner need; retained persistence proof
  follows its local [availability and decisions](architecture/persistence.md#decisions-recorded-here).
- Static musl image: stage 11 allocator decision.
- `mise`: the tool set outgrows Cargo + Go + Node + Docker.
- `cargo-vet` and a Renovate regex manager: a derived repository's policy.
- Multi-platform image: a consumer that deploys on another architecture;
  the Dockerfile is arch-neutral.
- Second `tower-http` in the tree (`axum-tracing-opentelemetry` through
  `reqwest`): a warning until upstream converges.
- BuildKit `rewrite-timestamp` for reproducible image ids: a consumer that
  needs identical image digests, not only identical binaries.

### Gotchas

1. `cargo deny check bans` treats path dependencies as wildcards; set
   `allow-wildcard-paths = true`.
2. `cargo deny check licenses` warns on an allowance no crate uses
   (`license-not-encountered`); add a license with the crate that brings it.
3. `gitleaks dir` ignores `.gitignore`; keep the `target/` allowlist.
4. zizmor is offline without a token; CI passes `GH_TOKEN`.
5. `actions/cache` with a save in a workflow that also runs on tags is a
   `cache-poisoning` finding; restore everywhere, save only on pushes to
   `main`. zizmor reads only `lookup-only` on `Swatinem/rust-cache`, not
   `save-if`, so its uses carry `zizmor: ignore[cache-poisoning]` beside a
   `save-if` that admits pushes to `main` alone.
6. `rust-toolchain.toml` in the image context makes rustup download
   components in every stage; exclude it and pin the `FROM` tag.
7. `cargo chef cook` must not run inside a cache mount when the layer is
   exported to `type=gha`; `cargo auditable build` after it reuses the
   cooked layer (`RUSTC_WORKSPACE_WRAPPER`).
8. Trivy's `rustbinary` analyzer needs the auditable section; zero Rust
   packages in a scan means the wrapper was dropped.
9. `go run github.com/zricethezav/gitleaks/v8@v8.30.1 version` prints
   "version is set by build process"; assert the tool by behaviour.
10. `GITHUB_ENV` accepts `NAME=value` lines only; strip the manifest's
    comments before appending it.
11. actionlint's shellcheck and pyflakes integrations use the host's
    binaries; `make actionlint` disables both so local and CI agree.
12. A `while read … do [[ … ]] && …; done` loop returns the last test's
    status; under `set -e` a final non-matching path aborts the step. Use
    `if`.
13. In zsh, `status` is a read-only variable; scripts run under `/bin/sh`.
14. The contract test that listed the exact operation ids broke on the first
    feature; it asserts probe presence now.
15. buildx's `type=gha` cache reads `ACTIONS_RUNTIME_TOKEN` and
    `ACTIONS_RESULTS_URL`, which GitHub hands to actions but not to `run:`
    steps, and it skips the import and export silently when they are absent
    (*verified*: the first `main` run of the `image` job showed neither
    "importing cache manifest from gha" nor "exporting cache"; the Go
    template's logs show the same). `crazy-max/ghaction-github-runtime`
    exposes them before `make runtime-image-build` in the publication
    action, the one path that still uses that cache.
16. An `actions/cache` entry is versioned by its path list as well as its
    key: a restore must name exactly the paths that were saved, or it misses
    every time without an error.
17. A tool that sets its own `CARGO_TARGET_DIR` compiles cold on every call;
    the initializer reuses an explicit absolute caller cache instead.
18. The first save claims a cache key, and later runs never replace it. Save
    a target cache only after the step that fills it; otherwise a push to
    `main` that ran only the OpenAPI, migration, or validation steps lets a
    partial target own the key until `Cargo.lock` changes.
    `Swatinem/rust-cache` evaluates `save-if` when the job starts, so the
    condition names the surfaces that select the filling step.
19. Cargo reads no environment variable for `[profile.dev.package."*"]`, and
    `Swatinem/rust-cache` does not hash the runner's Cargo configuration.
    Build settings belong in `Cargo.toml`, which the keys hash; a setting
    written to the runner's configuration needs a `CARGO_*` variable beside
    it, or the caches keep artifacts built at the old setting.

### Temporary OpenSSL runtime update

The pinned distroless Debian 13 image still carries `libssl3t64`
`3.5.7-1~deb13u2`; Trivy rejects CVE-2026-75804 and CVE-2026-84782.
The image overlays Debian security's exact `3.5.7-1~deb13u3` package and its
dpkg status record, downloaded through authenticated APT metadata in a separate
build stage. The runtime remains distroless and contains no package manager.
The Dockerfile owns this temporary package pin; remove the stage and copy when
a new pinned distroless image carries that version or later and Trivy passes.
Its exact APT package may later disappear from the active repository; this is
not a historical rebuild guarantee. The service's image maintainer owns that
manual retirement decision. Image lifecycle/security results establish the
combined image only for the candidate actually run; source CI alone is not a
published digest, published SBOM or live deployment receipt.
