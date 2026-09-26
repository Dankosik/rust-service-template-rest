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
version) and starts only the jobs its surfaces select:

| Job | Selected by | Proves |
| --- | --- | --- |
| `quality` | Rust source, manifests, lint config, OpenAPI, instructions, validation system | format; on pull requests clippy and tests of affected crates and dependents, on `main` and manifest changes the workspace; cargo-shear; Redocly and contract drift; oasdiff against the base; skills; validation self-tests |
| `security` | manifests, `deny.toml`, workflows; tool manifest and image on pull requests | cargo-deny (advisories, licenses, bans, sources); Dependency Review, fail on high, pull requests only; zizmor with the online audits |
| `secrets` | every event except a schedule without a policy change | Gitleaks over the commits since the base; the whole history on tags, manual runs, and a push without a readable base |
| `delivery` | shell, workflows, tool manifest, image, publication metadata | actionlint; ShellCheck over the changed scripts; `tools-check`; BuildKit Dockerfile checks; the publication metadata self-test |
| `image` | Docker/image sources and any selected profile image path | one local-default image: cached build, hardened lifecycle asserting `app.commit`, and Trivy for image changes; retained profile details below |
| `docs` | any `*.md`, `docs/`, `specs/` | every relative link and `#fragment` resolves (lychee, offline, pinned container); no toolchain |
<!-- template:begin grpc:docs-ci-grpc-gates -->
| `grpc` | schema, generated contracts, generator, Buf/compiler configuration | Buf format/lint, repeat deterministic generation, committed drift and FILE compatibility against the actual PR base; managed compiler resolver checks |
<!-- template:end grpc:docs-ci-grpc-gates -->
| `required` | always | fails when any job failed or was cancelled; requires terminal success for a selected initializer matrix |

<!-- template:begin postgres:docs-ci-postgres-gates -->
With PostgreSQL retained, `quality` also checks migration history and embedded
source rules. `integration` runs the real database proof on provider, migrator,
DB-test, Compose or DB-script changes. A migration change selects the image
rehearsal in place of plain lifecycle: `/migrate` against a fresh database,
`no_change` replay, then lifecycle with the pool open.
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
build and tests or the initializer itself. Eight parallel parts together run
`make template-init-check`. The source part proves the 368 baseline canonical profile
and harness projections. The 26 existing runtime graphs retain their public
initializer, build, test and selected database proof; their numbering and
baseline command scopes are unchanged.

Webhook graphs 27–46 add the five jobs-compatible authentication/idempotency
families, each with inbound-only (outbound HTTP absent or bounded), outbound-only,
and both directions. Every new graph runs the public initializer and actual
locked/offline Cargo metadata. Graphs 27, 29 and 30 run full build/test/database
proof once per new capability shape. The other 17 compile all retained test
targets and run the provider suite; inbound selections also run generated
contract and mounted inert-route process checks. These focused runs do not
repeat the full database suites.

The existing eight CI parts and warm-cache arrangement remain: source suites;
`database-none` (1–6); `database-postgres` (7–12); `http-idempotency` (13–16);
and four jobs parts containing baseline graphs 17–26 plus the new webhook
graphs. The exact graph IDs per part live in `ci.yml`; the runner records each
profile tuple, command, result and duration. Initializations within a part
reuse one absolute Cargo target. The five parts after `database-postgres`
restore its cache and save none; their retained full database suites require
Docker.

<!-- template:begin outbound-auth:docs-ci-outbound-auth-gates -->
OAuth adds four source projections and runtime graphs 50--53, without another
CI part or harness cross-product. The database-none part owns 50--52 (OAuth
alone, with JWT, with introspection); jobs-http-idempotency-2 owns the maximal
PostgreSQL graph 53. Graph 54 joins the database-none part for the
messaging/OAuth seam, and graph 55 joins jobs-http-idempotency-2 for the full
outbox/OAuth pack. Each runs initialization, locked metadata and compilation
of retained test targets; the workspace quality gate runs the OAuth behavior
suite. Database-free graphs do not request the removed integration-test feature.
<!-- template:end outbound-auth:docs-ci-outbound-auth-gates -->
Graph 56 joins jobs-1 for PostgreSQL/jobs/messaging without outbox. It uses the
focused locked offline metadata and all-target compile path with
`integration-tests/integration`, so the fixture callback's optional registry
argument is compiled. It adds no live PostgreSQL or NATS scenario; eight CI
parts cover 56 runtime representatives.

A change to projected text alone selects `module_initializer` without the
runtime surface and runs the Cargo-free `initializer (projections)` job.
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

[codeql.yml](../.github/workflows/codeql.yml) runs CodeQL for Rust
(`build-mode: none`) when Rust source or manifests change and for Actions
when workflows change, with `security-events: write` scoped to the analyze
jobs. The Rust analysis points the extractor's `cargo_target_dir` at a
persistent target restored with the crate registry; like the CI caches, only a
push to `main` writes it, after a successful analysis. `codeql-required`
accepts skipped analyses and rejects failed ones.

GitHub Rulesets or organization policy own merge admission: require
`required` and `codeql-required`. The repository does not rewrite its own
protection settings. Dependency Review needs the repository dependency graph
(Dependabot alerts enabled), an operator step for a derived repository.

## Generated contracts

The OpenAPI document is generated from the handlers and committed; the drift
test in `make test` refuses a stale copy, Redocly lints it, and pull requests
compare it with the base through oasdiff. Generated output is never edited by
hand ([Generated Contracts](validation/generated.md)).

## Secrets and dependencies

`.gitleaks.toml` carries the default rules and a `target/` allowlist; there
is no baseline file because the history is clean. `deny.toml` restricts the
graph to the two Linux gnu targets and lists the one advisory ignore with its
reopen condition. Duplicate crate versions are warnings, not failures.
Dependabot updates Cargo dependencies, GitHub Actions in workflows and in the
composite publication action, and the Dockerfile `FROM` digests; the tool
manifest is bumped by hand.

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

## Recovery

- Failed CI changes no external state; each job's containers are removed by
  the scripts' traps.
- A failed publication never promotes public tags; the run-scoped candidate
  tag is the only pushed reference.
- Rollback resolves a previously verified digest rather than rebuilding it.

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
| Advisories, licenses, bans, sources | `cargo-deny` 0.20.2; `[graph] targets` = the two Linux gnu triples; `allow-wildcard-paths = true`; `multiple-versions = "warn"`; one ignore (`RUSTSEC-2024-0436`, `paste` through `utoipa-axum` 0.2.0, reopened by its next release) | `cargo-audit` (same database, no reachability analysis exists for Rust); `cargo-vet` (a template cannot ship audits) | without `targets`, macOS-only crates enter license decisions about the Linux binary (*verified*); path dependencies count as wildcards otherwise (*verified*); `base64`, `getrandom`, `hashbrown`, `syn`, `tower-http` resolve twice through upstream crates and are warnings |
| Unused dependencies | `cargo-shear` 1.13.4 | `cargo-machete` (two false positives here: `humantime-serde` used through `#[serde(with)]`, `vergen-gitcl` in `build.rs`); `cargo-udeps` (nightly) | *verified*: cargo-shear found the unused `http` and `hyper` in `infra-http` with no false positive |
| Dependency metadata in the binary | `cargo auditable build` | plain `cargo build` | Trivy reports 178 Rust packages from the image's `/service` (`rustbinary`); without it the scan sees only Debian packages (*verified*) |
| New vulnerable dependencies | `actions/dependency-review-action` v5, `fail-on-severity: high`, pull requests only | — | needs the repository dependency graph (Dependabot alerts enabled), which was off here and had to be enabled (*verified* by the first failing run) |
| Secrets | Gitleaks 8.30.1 with the Go range/history policy; `.gitleaks.toml` = default rules + `target/` allowlist; no baseline | a baseline file | history is clean; `gitleaks dir` ignores `.gitignore` and read 153 MB of `target/` without the allowlist (*verified*) |
| Workflow syntax and security | actionlint 1.7.12 with host integrations off; zizmor 1.30.1 regular persona with `GH_TOKEN` for the online audits | — | zizmor found `cache-poisoning` on the original single-job workflow, fixed by restore-always/save-on-main (*verified*); its `dangerous-triggers` and `self-repository` findings on `cd.yml` are ignored inline with reasons (the job's `if` guards, and actionlint does not yet parse GitHub's `$/` form) |
| Dockerfile lint | `docker buildx build --check` | hadolint | BuildKit's linter is built in |
| Static analysis | CodeQL for Rust (GA since CodeQL 2.23.3) and Actions, `build-mode: none`, advanced setup with `codeql-required` | — | default setup was not configured, so the workflow does not conflict |
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
and the image rehearsal in place of the plain lifecycle check. Separate
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
| The initializer keeps eight parts, retaining the 26 baseline graphs and distributing 20 webhook graphs across the existing jobs parts | one sequential job or full database proof for every cross-profile combination | `required` reads one aggregate result; parts share the existing cache arrangement; three new full capability shapes plus 17 focused combinations bound duplicate builds and database execution; command-duration receipts expose changes to the critical path |
| `initializer_runtime` selects the matrix; projected text alone runs the Cargo-free projections | the whole matrix for every projected path | 250 of the 355 tracked files `module_initializer` selects are documentation, instructions, harness carriers or metadata, which cannot change a build; the projections prove their markers and harness independence in about a minute instead of the matrix's three |
| CI builds with `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, and every target cache key carries the level | full debuginfo | the `quality` and initializer caches were 4.2 and 4.1 GB, 8.7 of the repository's 10 GB, so the integration, Go-tool and buildx caches were evicted and one restore took 50–126 s; line tables keep file:line in test backtraces |
| `make verify` leaves heavy steps and the initializer matrix to CI and records a partial receipt | refusing to run without `ALLOW_HEAVY=1` or `ALLOW_FULL=1` | the refusal led agents to run the full matrix on a workstation, 40 minutes and more with several GB of temporary targets, while CI runs the same gates in parallel |
| No registry-only cache restore in `security` and CodeQL | the restores that were there | a cache version includes its path list, so restoring fewer paths than `quality` saves never hit (the CodeQL run of 2026-09-23 reported "Cache not found"); the crate downloads cost seconds |
| CodeQL Rust points the extractor's `cargo_target_dir` at a persistent target, cached with the registry under its own key | the default scratch directory per run | loading the workspace (crate downloads, build scripts, proc-macros, all features on) took about 60 s of a 7-minute analysis; codeql-action 4.38.1 offers neither dependency caching nor overlay analysis for Rust, and the queries (about 3 minutes) and database finalization (about 40 s) are fixed cost on a 4-vCPU runner |

### Runtime image

Two candidates were built cold from the tracked tree (*verified*,
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
`type=gha`; a source-only change rebuilds in about ten seconds.
`rust-toolchain.toml` stays out of the context because rustup would download
`clippy` and `rustfmt` in every stage (*verified*). Two `--no-cache` builds
produced byte-identical binaries (`CARGO_INCREMENTAL=0`, fixed `/src`,
`strip = true`, `SOURCE_DATE_EPOCH` for the mtime); image ids differ only by
layer timestamps. The lifecycle check runs from outside because the base has
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
<!-- template:begin jobs:docs-ci-jobs-worker-image -->
With the jobs pack retained the image also carries `/jobs-worker`, cooked and
built in the builder beside the main binary with its own `cargo chef cook` and
`cargo auditable build` steps, like `/migrate`. `ENTRYPOINT ["/service"]`
stays, and the worker runs as the same image with `--entrypoint /jobs-worker`.
`runtime-image-check.sh` adds a `/jobs-worker` step whose expectation comes
from the repository's jobs selection
([guide](background-jobs.md#run-and-stop-the-worker)).
<!-- template:end jobs:docs-ci-jobs-worker-image -->

### Publication and deployment

The composite action rather than a reusable workflow keeps the Fulcio
identity `…/.github/workflows/cd.yml@<ref>`; a release tag must equal
`v<crate version>` because Cargo owns the version. Config as Code
(`railway.toml`) is deprecated with a hard cutoff and closed to new services,
and `.railway/railway.ts` needs a `package.json` and a linked project a
template does not own, so the deployment policy is a document with an IaC
snippet ([Railway Deployment Profile](railway-deployment-profile.md)).
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
| `railway.toml` | profile document with an IaC snippet | Config as Code deprecated |
| Gitleaks baseline file | `target/` allowlist only | history is clean |
| `gotestsum` | `cargo test` | no present pressure for a runner |
| no link checker | lychee, offline, with fragments | the stage 5 exit criterion names one |
| `test/README.md` in the documentation graph | deferred to the first `test/` crate | no directory before its first artifact |

### Deferred, with the change that reopens each

- `cargo-nextest`: a measured test-runner need; retained persistence proof
  follows its local [availability and decisions](architecture/persistence.md#decisions-recorded-here).
- Static musl image: stage 11 allocator decision.
- `.railway/railway.ts` generation: an explicitly authorized deployment setup
  with owner-supplied project and provider inputs.
- `mise`: the tool set outgrows Cargo + Go + Node + Docker.
- `cargo-vet` and a Renovate regex manager: a derived repository's policy.
- Multi-platform image: a consumer that deploys on another architecture;
  the Dockerfile is arch-neutral.
- Second `tower-http` in the tree (`axum-prometheus`,
  `axum-tracing-opentelemetry` through `reqwest`): a warning until upstream
  converges.
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
   `main`.
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
    exposes them before `make runtime-image-build` in CI and in the
    publication action.
16. An `actions/cache` entry is versioned by its path list as well as its
    key: a restore must name exactly the paths that were saved, or it misses
    every time without an error.
17. A tool that sets its own `CARGO_TARGET_DIR` compiles cold on every call;
    the initializer reuses an explicit absolute caller cache instead.
18. The first save claims a cache key, and later runs never replace it. Save
    a target cache only after the step that fills it; otherwise a push to
    `main` that ran only the OpenAPI, migration, or validation steps lets a
    partial target own the key until `Cargo.lock` changes.
