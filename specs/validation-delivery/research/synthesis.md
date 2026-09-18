# Stage 4 research synthesis: validation routing and delivery

Decisions for roadmap stage 4 (changed-surface CI, security gates, one tool
manifest, production image, opt-in publication, `make verify`). Versions were
read from crates.io, npm, and GitHub on 2026-09-18. Claims marked *verified*
were executed on this workspace (Rust 1.98.1, Docker 29.4.0, macOS arm64,
scratch files under `/tmp`); claims marked *observed* were read in
documentation or source without being executed.

The Go template supplies the problems and the reasons behind its choices:
`tools/go.mod` + `go tool` as the one pin manifest, `changed-surfaces.sh`
fail-closed classification reused by CI and `make verify`, `required` accepting
deliberate skips, `govulncheck`/`gosec`/Dependency Review/Gitleaks/actionlint/
shellcheck/CodeQL where each observes something, a distroless image checked
from outside, and a publication that signs, attests, verifies, and only then
promotes tags. Where Rust solves a problem differently, the Rust way wins and
the deviation is recorded at the end.

## Requirements

From the roadmap stage and the Go reference (`.github/workflows/{ci,cd,codeql}.yml`,
`scripts/ci/*.sh`, `build/docker/Dockerfile`, `make/template.mk`,
`docs/validation-routing.md`, `docs/ci-cd-production-ready.md`):

- a docs-only pull request runs no Rust job; `required` fails on any failed or
  cancelled gate and accepts skips;
- the same classifier drives CI and the local `make verify` plan, fails closed
  on an unclassified path, and a pull request that changes the classifier is
  classified by both the base and the head version;
- pull-request Rust tests and lint select the affected crates; `main` runs the
  workspace;
- security gates run where they observe something: dependency advisories,
  licenses, bans, and sources; unused dependencies; new vulnerable
  dependencies on pull requests; secrets over the range (pull request) or
  history (tags, manual); workflow and shell static analysis; CodeQL for Rust;
- every tool version is pinned once and consumed by `make` and CI; a
  resolution check proves the pins resolve;
- a multi-stage, reproducible, non-root image with `STOPSIGNAL SIGTERM`, the
  commit baked in, a lifecycle check from outside (start, readiness, version,
  clean `SIGTERM` inside the grace budget), and a vulnerability scan that
  actually sees the application's dependencies;
- opt-in GHCR publication: run-scoped candidate, scan, SBOM, keyless signing,
  provenance and SBOM attestation, verification before tag promotion; nothing
  is published during this stage;
- `make verify` prints a plan and a receipt; `ALLOW_FULL` and `ALLOW_HEAVY`
  guard aggregate and heavy commands; `lint-changed` and `test-changed` take a
  crate list;
- allocator, LTO, codegen units, and PGO stay untouched (stage 11).

## Tool manifest

The Go template pins every developer tool in `tools/go.mod` and runs it with
`go tool`; the module checksum database verifies each download. Cargo has no
equivalent: `cargo install` builds from crates.io source (checksum-verified by
the registry index) and there is no per-project tool table.

| Candidate | Latest (date) | What it covers | Verdict |
| --- | --- | --- | --- |
| Plain versions file + `cargo install --locked --version` | — | Cargo tools; other ecosystems keep their own pinned one-liners (`npx pkg@v`, `go run mod@v`, `image:tag@sha256`) | **selected** as the manifest: `tools/versions.env`, `NAME=value` lines, read by `make` (`include`), by shell, and by CI (`>> $GITHUB_ENV`) with zero parsing code |
| `taiki-e/install-action` | v2.87.14 (2026-09-17) | 100+ tools as prebuilt release binaries with checksums recorded in the action's own manifests (*observed*: `cargo-deny`, `cargo-shear`, `cargo-machete`, `cargo-auditable`, `cargo-cyclonedx`, `cargo-nextest`, `zizmor`, `cosign`, `shellcheck`, `syft`, `trivy`; not `gitleaks`, `actionlint`, `oasdiff`) | **selected for CI installation** of the Cargo tools; versions come from the manifest through `env`. Compiling them in CI costs 6 min locally on an M-series laptop (*verified*, below) and more on a runner |
| `cargo-binstall` + versions file | 1.23.0 (2026-09-05) | Cargo tools as release binaries; checksum or signature verification only when the crate publishes `binstall` metadata | rejected as a required prerequisite: adds a bootstrap step on every workstation for a speed-up `cargo install` already gives once per version bump |
| `cargo-run-bin` (`[package.metadata.bin]`) | 1.7.5 (2025-07-14); last GitHub release 2024-11 | Cargo tools only, built into `.bin/` | rejected: Cargo-only, quiet since 2025 |
| `mise` (`mise.toml` + `mise.lock`) | v2026.9.11 (2026-09-18) | every ecosystem through `cargo:`, `npm:`, `go:`, `aqua:` backends; lockfile with checksums and provenance for `aqua`/`github` backends, version-only for `cargo`/`npm` (*observed* in `mise-lock` docs) | rejected for now: a new prerequisite for every workstation and agent, a monthly-release tool manager whose `cargo` backend either compiles or shells out to `cargo-binstall`, and `--locked` rejects version-only entries. Reopen if the tool set outgrows Cargo + Go + Node + Docker or cross-ecosystem checksum locking becomes a requirement |
| `tools/Cargo.toml` with tools as dependencies (Go `tools/go.mod` trick) | — | Dependabot would bump them | rejected: Cargo would compile `cargo-deny` as a library dependency; not how Cargo tools are consumed |

Per-ecosystem mechanics, one version source (`tools/versions.env`):

| Tool | Local | CI | Why this path |
| --- | --- | --- | --- |
| `cargo-deny`, `cargo-shear`, `zizmor` | `cargo install --locked --root <git-common-dir>/tools/<crate>-<version> --version <version> <crate>` as a Make prerequisite; idempotent (a second run is a no-op); the binary path is fixed, so no PATH or version-drift check is needed | `taiki-e/install-action` with `tool: cargo-deny@${{ env.CARGO_DENY_VERSION }},…` after `grep -v '^#' tools/versions.env >> "$GITHUB_ENV"` | crates.io source build locally (no extra prerequisite, registry checksums); checksum-verified prebuilt binaries in CI. Local compile cost (*verified*, M-series): `cargo-machete` 29 s, `cargo-shear` 58 s, `cargo-deny` 121 s, `zizmor` 161 s, once per version |
| `cargo-chef`, `cargo-auditable` | inside the Dockerfile builder stage: `cargo install --locked --version` with `ARG` defaults | same Dockerfile | Railway source builds pass no build arguments, so the Dockerfile must carry defaults; a self-test asserts the defaults equal the manifest |
| Redocly CLI | `npx --yes @redocly/cli@<version>` (already so) | same | unchanged; the pin moves from `make/template.mk` to the manifest |
| oasdiff, Gitleaks, actionlint | `go run <module>@v<version>` (Go module checksum database) | same, with `actions/cache` on the Go module and build caches keyed on the manifest | Go is already a prerequisite for oasdiff; `go run` of Gitleaks compiled in 16 s cold and 1 s warm (*verified*), actionlint 1 s warm. `install-action` covers neither Gitleaks nor actionlint |
| ShellCheck, Trivy | `docker run … koalaman/shellcheck:v0.11.0@sha256:…`, `aquasec/trivy:0.74.0@sha256:…` | same | Haskell and Go binaries with no Cargo or Go module path; the Go template already ran both as read-only pinned containers in CI; Docker is a prerequisite for image work anyway |
| cosign | consumers install it themselves to verify | `sigstore/cosign-installer` with `cosign-release: v${{ env.COSIGN_VERSION }}` | publication-only |

Resolution check (`make tools-check`, analog of `tools-resolution-check.sh`):
every Cargo tool named in the manifest installs (or is already installed) at
its version; the Dockerfile `ARG` defaults and `FROM rust:<version>` tag equal
the manifest and `rust-toolchain.toml`; every `*_IMAGE` value carries a
digest. The manifest is not Dependabot-managed; bumps are deliberate
commits (a derived repository may add a Renovate regex manager).

## Dependency and security gates

| Gate | Tool (version, date) | Observed on this workspace | Verdict |
| --- | --- | --- | --- |
| Advisories, licenses, bans, sources | `cargo-deny` 0.20.2 (2026-07-09) | *verified* with a draft `deny.toml`: `paste` 1.0.15 fails `advisories` (RUSTSEC-2024-0436, unmaintained, via `utoipa-axum` 0.2.0, still the latest release); path dependencies fail `bans` under `wildcards = "deny"` until `allow-wildcard-paths = true`; duplicate versions of `base64`, `getrandom`, `hashbrown`, `syn`, `tower-http` warn; licenses pass with the allow-list `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`, `Zlib`, `CC0-1.0` (`CC0-1.0` is `axum-tracing-opentelemetry` and `tracing-opentelemetry-instrumentation-sdk`; `r-efi`, `ryu`, `memchr`, `zerocopy` are dual-licensed and satisfied by `MIT`/`Apache-2.0`); `sources` pass with crates.io only | **selected**; `[graph] targets` restricted to the two Linux gnu triples so macOS-only crates (`libproc`, `mach2`, `bindgen`) do not enter the policy; `ignore = [{ id = "RUSTSEC-2024-0436", reason = "paste is a proc-macro used only by utoipa-axum 0.2.0; the unreleased utoipa-axum moves to pastey" }]` with the reopen condition "next `utoipa-axum` release" |
| Advisories again | `cargo-audit` 0.22.2 (2026-06-05) | same RustSec database as `cargo-deny advisories`; adds `cargo audit bin` for `cargo-auditable` binaries and `audit fix` | rejected as a second gate: no reachability analysis exists for Rust (Go's `govulncheck` has no counterpart), so the two tools observe the same lockfile. Trivy scans the auditable binary in the image instead |
| Unused dependencies | `cargo-shear` 1.13.4 (2026-08-11) | *verified*: reports `http` and `hyper` unused in `infra-http` plus their `[workspace.dependencies]` rows; no false positives | **selected** (Cargo has no `go mod tidy -diff`); the two findings are fixed in this stage |
| Unused dependencies | `cargo-machete` 0.9.2 (2026-04-15) | *verified*: same two findings plus two false positives: `humantime-serde` (used through `#[serde(with = "humantime_serde")]`) and `vergen-gitcl` (used in `build.rs`) | rejected: regex matching misses string paths and build scripts, which this workspace uses |
| Unused dependencies | `cargo-udeps` 0.1.61 | needs nightly | rejected: toolchain pin is stable |
| Supply-chain audits | `cargo-vet` 0.10.2 (2026-01-13; last GitHub release 2024-10) | per-crate audit records | rejected: a template cannot ship meaningful audits; a derived repository with the policy may adopt it |
| Dependency metadata in the binary | `cargo-auditable` 0.7.6 (2026-09-13) | *verified*: Trivy reports 178 Rust packages from the image's `/service` (`Type: rustbinary`), including `paste 1.0.15`; without it the image scan sees only Debian packages | **selected**: `cargo auditable build` in the Dockerfile; it uses `RUSTC_WORKSPACE_WRAPPER`, so the `cargo chef cook` layer stays valid (*verified*: final build 9.9 s after a 61.8 s cook) |
| New vulnerable dependencies on pull requests | `actions/dependency-review-action` v5.0.0 (2026-05-08) | *observed*: dependency graph supports `Cargo.lock`; the repository is public, so the graph is on; Dependabot alerts are disabled at the repository (operator setting, not required by the action) | **selected**, `fail-on-severity: high`, pull requests only |
| Secrets | Gitleaks 8.30.1 (2026-03-21) via `go run github.com/zricethezav/gitleaks/v8@v8.30.1` | *verified*: worktree and full history clean, so no baseline file; `gitleaks dir .` scanned 646 MB because it walked `target/` | **selected** with the Go range/history policy; `.gitleaks.toml` carries `useDefault = true` and a path allowlist for `target/` (the only entry with a current reason); no `.gitleaks.baseline.json` until a finding needs one |
| Workflow syntax | actionlint 1.7.12 (2026-03-30) via `go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12` | *verified*: current `ci.yml` clean | **selected** |
| Workflow security | zizmor 1.30.1 (2026-09-09) | *verified*: one `high` finding on the current `ci.yml`: `cache-poisoning` (`actions/cache` with save in a workflow that also runs on `v*` tags); two `informational` under `--persona pedantic`. Offline by default without a token; online audits (`known-vulnerable-actions`, `impostor-commit`) with `GH_TOKEN` | **selected**, regular persona; CI passes `GH_TOKEN`; the finding is fixed in this stage by the Go pattern (restore always, save only on pushes to `main`) |
| Shell | ShellCheck 0.11.0 (2025-08-04) | image `koalaman/shellcheck:v0.11.0@sha256:61862eba1fcf09a484ebcc6feea46f1782532571a34ed51fedf90dd25f925a8d` | **selected** |
| Dockerfile lint | BuildKit `docker buildx build --check` | *verified*: candidate Dockerfile passes | **selected**; hadolint (2.15.1) not added, BuildKit's linter is built in |
| Static analysis | CodeQL for Rust, GA since CodeQL 2.23.3 (2025-10-14) | *observed*: default setup on this repository is `not-configured` with languages `actions`, `python`, `rust` detected, so an advanced `codeql.yml` does not conflict | **selected**: `codeql.yml` with `rust` and `actions`, `build-mode: none`, same `changes` classification, `codeql-required` job |
| Image vulnerabilities | Trivy 0.74.0 (2026-08-14), image `aquasec/trivy:0.74.0@sha256:62b1e65e8869bc4b4c6aa4fa2b21595256c7c2f6018a9d9ad61caf87187c1969` | *verified* on both candidate images (below) | **selected** (`--severity HIGH,CRITICAL --ignore-unfixed --exit-code 1`), same flags as the Go target; grype 0.119.0 also reads `cargo-auditable` but adds a second scanner for no new observation |
| SBOM | Trivy `--format cyclonedx` on the pushed image | describes the shipped artifact: base packages plus the auditable Rust list | **selected**; `cargo-cyclonedx` 0.5.9 and `cargo-sbom` 0.10.0 describe the source graph (host-only crates included unless filtered) rather than the image; `syft` 1.52.0 would be a second scanner |
| Signing and attestation | cosign v3.1.3 keyless, `actions/attest` v4.2.2 for provenance and SBOM | identical to the Go composite action | **selected** |

## CI routing

| Candidate | Latest | Verdict |
| --- | --- | --- |
| Port `scripts/ci/changed-surfaces.sh` + `git-changed-paths.sh` | — | **selected**. Named gap: no action classifies fail-closed, reports unclassified paths, unions the base commit's classifier with the head's, and runs unchanged under `make verify` and `make plan` |
| `dorny/paths-filter` | v4.0.3 (2026-08-05) | rejected: CI-only, no unclassified-path failure, no base-classifier union |
| `tj-actions/changed-files` | v47.0.6 (2026-04-18) | rejected: same, and it lists files rather than deciding surfaces |

Surfaces for this repository (the Go list with Go-only surfaces dropped and
Rust owners named; the self-test in the script pins each row):

| Surface | Paths | Selects |
| --- | --- | --- |
| `rust_source` | `crates/**/*.rs`, `crates/*/tests/**`, `env/config/*` | affected-crate lint and tests on pull requests; workspace lint and tests on `main`; `cargo-shear` |
| `cargo_dependencies` | `Cargo.toml`, `Cargo.lock`, `crates/*/Cargo.toml`, `rust-toolchain.toml` | workspace lint, build, tests; `cargo-deny`; `cargo-shear`; Dependency Review; CodeQL Rust |
| `dependency_policy` | `deny.toml` | `cargo-deny` |
| `lint_config` | `clippy.toml`, `rustfmt.toml`, `Cargo.toml` (workspace lints) | workspace format and lint |
| `openapi` | `api/openapi/*`, `.redocly.yaml` | `service` contract tests, Redocly lint, oasdiff on pull requests |
| `tool_manifest` | `tools/versions.env` | `tools-check` |
| `github_workflows` | `.github/workflows/*`, `.github/actions/**` | actionlint, zizmor, Dependency Review, CodeQL Actions |
| `dependency_automation` | `.github/dependabot.yml` | nothing (GitHub validates it) |
| `shell` | `**/*.sh` | ShellCheck (changed files on diff events) |
| `runtime_image` | `build/docker/*`, `.dockerignore`, `scripts/ci/runtime-image-*.sh` | Dockerfile check, image build, lifecycle check, Trivy |
| `publication_metadata` | `.github/actions/publish-image/*`, `scripts/ci/publish-image-metadata.sh` | metadata self-test |
| `secret_scanning` | `.gitleaks.toml` | Gitleaks (always runs anyway) |
| `agent_instructions` | `AGENTS.md`, `CLAUDE.md`, `.agents/**`, `docs/skill-authoring.md`, `scripts/check-skills.py` | `check-skills` |
| `documentation` | `**/*.md`, `docs/**`, `specs/**` | nothing (no repository-wide documentation validator yet; stage 5 adds the link checker) |
| `validation_system` | `Makefile`, `make/*.mk`, `scripts/ci/{changed-surfaces,git-changed-paths,affected-crates,verify,validation-lock,measure}.sh` | the scripts' self-tests |
| `no_validation_required` | `.editorconfig`, `.gitattributes`, `.gitignore`, `LICENSE`, `.github/CODEOWNERS`, `.github/ISSUE_TEMPLATE/*`, `.github/pull_request_template.md` | nothing |

Affected crates (port of `affected-go-packages.sh` as `scripts/ci/affected-crates.sh`):
a changed path under `crates/<dir>/` maps to the package named in that
directory's `Cargo.toml`; the reverse closure comes from
`cargo tree --locked --workspace -i <package> -e normal,build,dev --prefix none`
(*verified*: `health` → `infra-http`, `service`; `service-config` →
`service`), so a dev-dependency edge also reselects the dependent's tests.
Any `Cargo.toml`, `Cargo.lock`, or `rust-toolchain.toml` change, or a Rust
file outside `crates/`, falls back to the workspace: feature unification can
change an unrelated crate's compiled features. Output: `lint_packages`,
`test_packages`, `fallback`, consumed as `cargo clippy -p a -p b …` and
`cargo test -p a -p b …` by `make lint-changed PKGS=…` and
`make test-changed PKGS=…`. Formatting always checks the whole workspace
(sub-second).

Test runner: `cargo test` stays. `cargo-nextest` 0.9.145 (2026-09-16) was
assessed against the pressures it relieves: per-test process isolation (no
test here mutates the process environment; `std::env::set_var` is `unsafe` in
edition 2024 and `unsafe_code` is forbidden), per-test timeouts (every wait in
the process tests is bounded; the job timeout is the outer bound), JUnit and
partitioning (80 tests, seconds). None is present today; adopting it would
make every `make test` depend on a compiled tool. Reopen with stage 8's
Docker-backed integration tests, where retries, per-test timeouts, and
isolation have a concrete owner.

Job layout, one `changes` job feeding conditional jobs and an always-reported
`required`:

| Job | Runs when | Steps |
| --- | --- | --- |
| `changes` | always | classify (`--all` on tags, schedule, dispatch; diff + `--union BASE` otherwise) |
| `quality` | `rust_source`, `cargo_dependencies`, `lint_config`, `openapi`, `agent_instructions`, `validation_system` | toolchain and cache only when a Rust surface is selected; format; affected or workspace clippy, build, tests; `cargo-shear`; OpenAPI lint and pull-request compatibility; skills; validation-system self-tests |
| `security` | `cargo_dependencies`, `dependency_policy`, `github_workflows`; every surface on schedule, tags, and dispatch (advisory databases move without a commit); Dependency Review additionally on pull requests touching `runtime_image` or `tool_manifest` | `cargo-deny`; Dependency Review; zizmor |
| `secrets` | always except schedule without `secret_scanning` | Gitleaks range or history |
| `delivery` | `shell`, `github_workflows`, `runtime_image`, `publication_metadata`, `tool_manifest` | actionlint; ShellCheck; `docker buildx build --check`; metadata self-test; `tools-check` |
| `image` | `runtime_image` | build, lifecycle check, Trivy (one image, three observations) |
| `required` | `always()` | fail on `failure`/`cancelled` in `needs.*.result`; accept `skipped` |

Caches: `actions/cache/restore` on every run, `actions/cache/save` only on
pushes to `main` (fixes the zizmor `cache-poisoning` finding on the current
workflow and matches the Go layout).

## Runtime image

Two candidate Dockerfiles were built from the tracked tree (*verified*, cold,
`--no-cache`, `linux/arm64`, Docker 29.4.0):

| Variant | Builder | Runtime base | Cold build | Image | Binary | Trivy (OS) | Trivy (Rust) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| glibc | `rust:1.98.1-slim-trixie@sha256:3999a7ff854f315cf5f2b9a58071cb71196fdfc2ccd32fa20eedce8e754fd62a` | `gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97` | 150 s (tools 52 s, `chef cook` 62 s, final build 10 s) | 43.5 MiB (base ≈ 36.5 MiB) | 7.0 MiB, dynamic | 14 packages, 20 CVEs, all unfixed, 0 fixed HIGH/CRITICAL | 178 packages, 0 CVEs |
| musl | same + `rustup target add aarch64-unknown-linux-musl` (16 s) | `gcr.io/distroless/static-debian13:nonroot@sha256:e2e927ec666bae08560abb3c55d0659eceabb657f56b6782ab500a9fc7f555e3` | 139 s (tools 41 s, target 16 s, `chef cook` 66 s, final build 12 s) | 9.1 MiB (base ≈ 2.3 MiB) | 6.8 MiB, static | 6 packages, 0 CVEs | 178 packages, 0 CVEs |

Both pass the lifecycle probe (*verified* on the glibc image, `--read-only
--cap-drop=ALL --security-opt=no-new-privileges`, no config file): ready on
the first poll, `app.commit` equals the `VCS_REF` build argument, `docker stop
--time 45` exits `0`. The dependency tree has no TLS or C library (OTLP is
plain HTTP), which is why the musl build needed nothing beyond the target.

Decision: **glibc on `distroless/cc-debian13`** for this stage.

- Build time is equal and the musl image is 34 MiB smaller with a cleaner
  scan; those numbers are recorded, not disputed.
- musl replaces the allocator. Its malloc is the known weak point of
  multi-threaded Rust services, and the standard remedy is a custom global
  allocator, which stage 11 owns ("allocator, LTO, PGO from measurements").
  Choosing musl now would pre-decide that measurement.
- The glibc image runs the same target triple the tests run on
  (`*-unknown-linux-gnu` locally and on `ubuntu-latest`); what CI tested is
  what ships.
- The 20 unfixed Debian CVEs are excluded by `--ignore-unfixed`, as in the Go
  target; they remain visible in the SBOM.

Reopen with stage 11: if it adopts a custom allocator, the static image
becomes the default; the switch is the two `--target` lines and the base
digest, already verified above. `debian13` (trixie, current stable) over
`debian12`: the builder and runtime must share a glibc generation, and the
`rust:1.98.1-slim-trixie` builder pairs with `cc-debian13` (a trixie-built
binary does not start on `cc-debian12`).

| Area | Decision | Evidence |
| --- | --- | --- |
| Dependency layer cache | `cargo-chef` 0.1.78 (`prepare` in a planner stage, `cook` in the builder, no cache mounts) | BuildKit does not export `RUN --mount=type=cache` to `--cache-to type=gha` (*observed*, Docker docs and moby/buildkit#3011); the cooked layer is a plain layer, so `type=gha,mode=max` restores it and a source-only change rebuilds in ~10 s (*verified* locally). `buildkit-cache-dance` v3.4.0 would be a second mechanism to persist mounts |
| Chef and auditable in the builder | official `rust` image + `cargo install --locked --version` (52 s, cached as a layer until a version or base digest changes) | the prebuilt `lukemathwalker/cargo-chef` image is a third-party base; the official image keeps the trust root in Docker Hub's `rust` plus crates.io |
| Toolchain inside the image | `.dockerignore` excludes `rust-toolchain.toml`; the `FROM rust:<version>` tag is the toolchain authority; `tools-check` asserts it equals the `channel` | *verified*: with the file present, rustup downloaded `clippy` and `rustfmt` in the planner and builder stages ("syncing channel updates", "downloading component clippy"); Go had `GOTOOLCHAIN=local` for the same reason |
| Reproducibility | `CARGO_INCREMENTAL=0`, `strip = true` (already in the release profile), fixed `/src` path, `SOURCE_DATE_EPOCH` for the binary's mtime and for BuildKit's `rewrite-timestamp` | *verified*: two independent `--no-cache` builds produced byte-identical binaries (`sha256 e6c88dee…`); image IDs differed only in layer timestamps. `--remap-path-prefix` is not needed for reproducibility (paths are fixed in the container); it would only shorten panic-location strings |
| Version and commit | `app.version` stays the Cargo package version; `ARG VCS_REF` → `ENV VERGEN_GIT_SHA` bakes the commit; `RAILWAY_GIT_COMMIT_SHA` wins when present | the Go `sha-<12>` *version* was a workaround for having no module version; Cargo owns the version, and the lifecycle check asserts `app.commit` |
| Labels | `org.opencontainers.image.{version,revision,source}` from build arguments | as Go |
| Lifecycle check | `scripts/ci/runtime-image-check.sh`: `docker run -d -p 127.0.0.1::8080 --read-only --cap-drop=ALL --security-opt=no-new-privileges`, poll `/health/ready` from the host, assert `app.commit` in the first log line, `docker stop --time 45`, exit `0` | distroless has no shell or curl; the Go check works the same way. `APP__HTTP__READINESS_PROPAGATION_DELAY` stays at its default so the check also observes the 15 s delay inside the 45 s budget |
| Image build entry | `scripts/ci/runtime-image-build.sh` (`docker buildx build --load`, optional `--cache-from/--cache-to`, build arguments from the manifest) | one command for `make`, CI, and CD |
| Dependabot | `docker` ecosystem for `/build/docker`, `github-actions` for `/.github/actions/publish-image` | digests in `FROM` lines are Dependabot-managed; the tool manifest is not |

## Publication

`cd.yml` is the Go workflow with the migration steps removed: `workflow_run`
of `ci` on `main` plus `push` of `v*` tags; `if: vars.ENABLE_GHCR_PUBLISH ==
'true'`; waits for the exact-SHA `codeql.yml` run (and `ci.yml` for tags);
checks out the candidate SHA; calls the composite action
`.github/actions/publish-image` (composite rather than reusable workflow so the
Fulcio identity stays `…/.github/workflows/cd.yml@<ref>`):

1. metadata (`scripts/ci/publish-image-metadata.sh`, ported with its
   self-test; `main` mode promotes `sha-<12>` and `main`, `release` promotes
   the tag and `latest`);
2. GHCR login, cosign install, Buildx;
3. `make runtime-image-build` with `type=gha` cache;
4. `make runtime-image-check` (the rehearsal the Go action ran through
   `migration-validate`);
5. `make container-security` (Trivy) and Trivy CycloneDX SBOM;
6. push the run-scoped candidate, resolve the digest;
7. `cosign sign` keyless; `actions/attest` provenance and SBOM;
8. `cosign verify` with the exact identity, `gh attestation verify` for both
   predicates;
9. summary, SBOM artifact upload;
10. `promote` with digest read-back per tag.

Nothing runs in this stage: the variable stays unset and no `v*` tag is
created. The workflow is validated by actionlint, zizmor, and the metadata
self-test; the publication path itself is proven at stage 12.

## Deployment profile

Railway deprecated Config as Code (`railway.toml`/`railway.json`) in favour of
Infrastructure as Code (`.railway/railway.ts`, `railway` npm package 3.11.0,
2026-08-24). *Observed* in the current Railway documentation: "New services
cannot opt into Config as Code. Existing Config as Code files stop being read
on 2026-12-01 (hard cutoff)." IaC is evaluated by the Railway CLI against a
linked project and applied explicitly; Railway does not read `.railway/`
during deploys. The `service()` config carries every setting the Go
`railway.toml` used (*observed* in the package's type declarations:
`build.builder`, `build.dockerfilePath`, `build.watchPatterns`,
`deploy.healthcheckPath`, `deploy.healthcheckTimeout`,
`deploy.restartPolicyType`, `deploy.restartPolicyMaxRetries`,
`deploy.overlapSeconds`, `deploy.drainingSeconds`).

Decision: **no `railway.toml`**, and no `.railway/railway.ts` either. The
former would be a dead format for every new service; the latter needs a
`package.json` with the `railway` dependency, a project name, and a linked
project, none of which a template owns. `docs/railway-deployment-profile.md`
carries the deployment policy as the values and a copy-ready `service()`
snippet (Dockerfile path, watch patterns for the Rust source set,
`/health/ready`, `healthcheckTimeout 180`, `ON_FAILURE`/5, `overlapSeconds
45`, `drainingSeconds 45`, the 42 s worst-case derivation). Reopen at stage 9:
the initializer knows the service identity and may generate `.railway/`.

## `make verify`, guards, and the validation system

| Piece | Decision |
| --- | --- |
| `scripts/ci/verify.sh` | ported: plan from surfaces, `--plan`, candidate fingerprint, receipt under `<git-common-dir>/codex/verify`, attempt record, step invalidation when the tree changes, `ALLOW_HEAVY`/Docker/binary preflight, `--self-test`. Command kinds reduce to `make`, `lint`, `test`, `shell`, `image-build`, `image-check`, `image-security` |
| `scripts/ci/validation-lock.sh` | ported unchanged (one Git-common lock; AGENTS.md forbids concurrent CPU-heavy validation) |
| `scripts/ci/measure.sh` | ported unchanged (per-step wall, CPU, RSS into the step summary) |
| `ALLOW_FULL` | guards `make check` only; `make build`, `make test`, `make lint` stay the ordinary commands AGENTS.md names |
| `ALLOW_HEAVY` | guards `runtime-image-build`, `runtime-image-check`, `container-security`, `secret-scan-history`, and any `verify` plan containing them; `CI=true` satisfies both guards, as in Go |
| `lint-changed PKGS=…`, `test-changed PKGS=…` | `cargo clippy -p … --all-targets -- -D warnings`, `cargo test -p …`; `PKGS` is what `affected-crates.sh` prints |
| New targets | `tools-check`, `deny`, `unused-deps`, `secret-scan`, `secret-scan-history`, `actionlint`, `zizmor`, `shellcheck`, `dockerfile-check`, `runtime-image-build`, `runtime-image-check`, `container-security`, `publish-image-metadata-check`, `changed-surfaces-check`, `affected-crates-check`, `validation-lock-self-test`, `verify-check`, `plan`, `verify` |

## Version set

| Pin | Value |
| --- | --- |
| `cargo-deny` | 0.20.2 |
| `cargo-shear` | 1.13.4 |
| `zizmor` | 1.30.1 |
| `cargo-chef` | 0.1.78 |
| `cargo-auditable` | 0.7.6 |
| Redocly CLI | 2.53.3 |
| oasdiff | 1.32.1 |
| Gitleaks | 8.30.1 |
| actionlint | 1.7.12 |
| ShellCheck image | `koalaman/shellcheck:v0.11.0@sha256:61862eba1fcf09a484ebcc6feea46f1782532571a34ed51fedf90dd25f925a8d` |
| Trivy image | `aquasec/trivy:0.74.0@sha256:62b1e65e8869bc4b4c6aa4fa2b21595256c7c2f6018a9d9ad61caf87187c1969` |
| cosign | 3.1.3 |
| Builder image | `rust:1.98.1-slim-trixie@sha256:3999a7ff854f315cf5f2b9a58071cb71196fdfc2ccd32fa20eedce8e754fd62a` |
| Runtime base | `gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97` |
| Actions (pinned by SHA at implementation) | `actions/checkout` v7.0.1, `actions/cache` v6.1.0, `taiki-e/install-action` v2.87.14, `actions/dependency-review-action` v5.0.0, `github/codeql-action` v4, `docker/setup-buildx-action` v4.4.1, `sigstore/cosign-installer` v4.1.2, `actions/attest` v4.2.2, `actions/upload-artifact` v7.0.1, `aquasecurity/trivy-action` v0.36.0 |

## Deviations from the Go template

| Go template | Rust template | Why |
| --- | --- | --- |
| `tools/go.mod` + `go tool`, one mechanism locally and in CI | `tools/versions.env`; `cargo install --locked` into the Git-common tool root locally, `taiki-e/install-action` in CI; Go tools through `go run`; Haskell and Go binaries as pinned containers | Cargo has no project-local tool table or checksum-verified binary distribution; compiling four tools costs six minutes per version bump and is acceptable once per workstation, not per CI run |
| `govulncheck` (reachable vulnerabilities) + `gosec` | `cargo-deny` (advisories, licenses, bans, sources) + CodeQL Rust | no reachability analysis exists for Rust; `cargo-audit` would duplicate the advisory check; `gosec`'s role is CodeQL's |
| `go mod tidy -diff`, `go mod verify` | `cargo-shear`; `--locked` on every Cargo command | Cargo has no tidy; the lockfile is verified by `--locked` |
| Native Go binary in a static distroless image, `GOTOOLCHAIN=local` | glibc binary on `distroless/cc-debian13`, `rust-toolchain.toml` excluded from the image context | allocator neutrality until stage 11 and target parity with the tests; rustup would otherwise download components at build time |
| Go binaries carry module metadata natively | `cargo auditable build` | Rust binaries carry nothing by default; without it the image scan observes only Debian packages |
| `APP_VERSION=sha-<12>` baked as the version | version from Cargo, commit from `VCS_REF` | Cargo owns the version; the check asserts the commit |
| Go build cache as a `RUN --mount=type=cache` | `cargo-chef` layers, no cache mounts | mounts are not exported to the GitHub Actions cache; Rust dependency compilation is the dominant cost |
| `railway.toml` | deployment profile document with an IaC `service()` snippet | Config as Code is deprecated with a hard cutoff and closed to new services; IaC is applied by the CLI against a linked project |
| `gitleaks dir .` with Go-specific allowlists and a baseline file | `gitleaks dir .` with a `target/` path allowlist, no baseline | the only current reason for configuration is the build directory; history is clean |
| `gotestsum` runner | `cargo test` | no present pressure for `cargo-nextest`; reopen at stage 8 |
| Surfaces for protobuf, sqlc, initializers, integration records, migrations, compose, performance harness | absent | their owners do not exist yet; each stage adds its surface with its first artifact |

## Deferred, with the change that reopens each

- `cargo-nextest`: stage 8 integration tests.
- Static musl image: stage 11 allocator decision.
- `.railway/railway.ts` generation: stage 9 initializer.
- `mise` as a polyglot tool manager: the tool set outgrows Cargo + Go + Node +
  Docker.
- `cargo-vet`: a derived repository with an audit policy.
- Renovate regex manager for `tools/versions.env`: a derived repository that
  wants automated tool bumps.
- Multi-platform image (`linux/amd64,linux/arm64`): a consumer that deploys on
  arm64; the Dockerfile is arch-neutral, only the publication step changes.
- Second `tower-http` version in the tree (`axum-prometheus` or
  `axum-tracing-opentelemetry`): observed by `cargo-deny bans` as a warning;
  resolve when the upstream crates converge.

## Gotchas carried into implementation

1. `cargo deny check bans` treats `{ path = "…" }` dependencies as wildcards;
   set `allow-wildcard-paths = true` rather than adding versions to path
   dependencies (*verified*).
2. `cargo-deny` evaluates every target by default; `[graph] targets` keeps
   macOS-only crates (`libproc`, `mach2`, `bindgen`) out of license and
   advisory decisions about the shipped binary (*verified*).
3. `gitleaks dir` ignores `.gitignore`; without the `target/` allowlist a local
   scan reads hundreds of megabytes of build output (*verified*).
4. zizmor is offline without a token and skips `known-vulnerable-actions` and
   `impostor-commit`; CI must pass `GH_TOKEN: ${{ github.token }}`.
5. `actions/cache` with save in a workflow that also runs on tag pushes is a
   `cache-poisoning` finding; use `restore` everywhere and `save` only on
   pushes to `main` (*verified*).
6. `rust-toolchain.toml` inside the image context makes rustup download
   `clippy` and `rustfmt` in every stage that runs cargo; exclude it and pin
   the `FROM` tag (*verified*).
7. Builder and runtime images must share a Debian release: a trixie-built
   glibc binary does not start on `cc-debian12`.
8. `cargo chef cook` must not run inside a cache mount when the layer is
   meant to be exported to `type=gha`.
9. `cargo auditable build` after `cargo chef cook` does not rebuild
   dependencies (`RUSTC_WORKSPACE_WRAPPER`), so the cook layer is reused
   (*verified*).
10. Trivy's `rustbinary` analyzer needs the auditable section; a plain
    `cargo build` binary yields zero Rust packages in the image scan.
11. `go run github.com/zricethezav/gitleaks/v8@v8.30.1 version` prints
    "version is set by build process"; assert the tool by behaviour, not by
    its version string.
12. In zsh, `status` is a read-only variable; scripts that run under
    `SHELL := /bin/sh` are unaffected, but do not use that name in
    interactive verification.
13. `gcr.io/distroless/*:nonroot` tags are mutable; pin the digest and let
    Dependabot's `docker` ecosystem move it.
14. `GITHUB_ENV` accepts `NAME=value` lines only; strip comments from
    `tools/versions.env` before appending it.
15. Dependabot alerts are disabled on this repository; Dependency Review does
    not need them, but enabling them is an operator step worth documenting.
16. `cargo deny check licenses` warns (`license-not-encountered`) on an
    allowance no crate in the target-restricted graph carries; `ISC` from the
    draft list above was unmatched, so the committed `deny.toml` omits it and
    a crate that brings a new license adds its line in the same change
    (*verified*).
17. actionlint's shellcheck and pyflakes integrations run whatever binary the
    host has on PATH (a runner-image update could change the result);
    `make actionlint` disables both so local and CI agree, and `*.sh` files
    get the pinned ShellCheck container through `make shellcheck`.
