# CI/CD Production Readiness

What the pipeline proves about a candidate, in the order a change meets it,
and the decisions behind each gate with the alternatives they beat
([recorded at the end](#decisions-recorded-here)).

## Local leaves

Ordinary local completion is a matching build and relevant tests under
[AGENTS.md](../AGENTS.md#validation-budget); [Validation Routing](validation-routing.md)
selects anything beyond that. `make verify` runs the surface-aware route and
records a receipt; `ALLOW_FULL=1 make check` is the explicit full gate. Local
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
| `quality` | Rust source, manifests, lint config, OpenAPI, instructions, validation system | format; on pull requests clippy and tests of the affected crates and their dependents, on `main` and on any manifest change the workspace; cargo-shear; Redocly lint plus the drift and contract tests; oasdiff against the base; skills; the validation scripts' self-tests |
| `security` | manifests, `deny.toml`, workflows; tool manifest and image on pull requests | cargo-deny (advisories, licenses, bans, sources); Dependency Review, fail on high, pull requests only; zizmor with the online audits |
| `secrets` | every event except a schedule without a policy change | Gitleaks over the commits since the base; the whole history on tags, manual runs, and a push without a readable base |
| `delivery` | shell, workflows, tool manifest, image, publication metadata | actionlint; ShellCheck over the changed scripts; `tools-check`; BuildKit Dockerfile checks; the publication metadata self-test |
| `image` | `build/docker/*`, `.dockerignore`, the image scripts | one `service:ci` image: build (layers restored from the Actions cache, written only by pushes to `main` and schedules), hardened lifecycle check asserting `app.commit`, Trivy |
| `docs` | any `*.md`, `docs/`, `specs/` | every relative link and `#fragment` resolves (lychee, offline, pinned container); no toolchain |
| `required` | always | fails when any job failed or was cancelled; accepts skipped jobs |

A weekly schedule and manual dispatch select every surface, because advisory
databases move without a commit. Tags select every surface too. A docs-only
pull request runs `changes`, `docs`, `secrets`, and `required` and no Rust job
(verified on [#11](https://github.com/Dankosik/rust-service-template-rest/pull/11)
before the `docs` job existed; it adds a link check, not a toolchain).
Every action is pinned by commit SHA with its version beside it; tool
versions come from `tools/versions.env` through `GITHUB_ENV` and
`taiki-e/install-action`.

[codeql.yml](../.github/workflows/codeql.yml) runs CodeQL for Rust
(`build-mode: none`) when Rust source or manifests change and for Actions
when workflows change, with `security-events: write` scoped to the analyze
jobs. `codeql-required` accepts skipped analyses and rejects failed ones.

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
| `ALLOW_FULL` guards `make check` only; `ALLOW_HEAVY` guards the image targets and the history scan; `CI=true` satisfies both | guarding `lint` and `test` too | `make build`, `make test`, and `make lint` are the ordinary commands AGENTS.md names |

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

### Publication and deployment

The composite action rather than a reusable workflow keeps the Fulcio
identity `…/.github/workflows/cd.yml@<ref>`; a release tag must equal
`v<crate version>` because Cargo owns the version. Config as Code
(`railway.toml`) is deprecated with a hard cutoff and closed to new services,
and `.railway/railway.ts` needs a `package.json` and a linked project a
template does not own, so the deployment policy is a document with an IaC
snippet ([Railway Deployment Profile](railway-deployment-profile.md)); the
stage 9 initializer may generate `.railway/`.

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

- `cargo-nextest`: stage 8 integration tests.
- Static musl image: stage 11 allocator decision.
- `.railway/railway.ts` generation: stage 9 initializer.
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
