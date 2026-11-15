# CI/CD Production Readiness

What the pipeline proves about a candidate, the decisions behind each gate
([recorded below](#decisions-recorded-here)), and the runbooks for red `main`
and failed runs.

## Local leaves

[AGENTS.md](../AGENTS.md#validation-budget) owns ordinary local completion;
[Validation Routing](validation-routing.md) owns `make plan` / `make verify`
and explicit verification leaves. Local completion never proves platform
admission, runtime behavior, or a release.

## Pipeline contract

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml),
[`.github/workflows/cd.yml`](../.github/workflows/cd.yml),
[`.github/workflows/codeql.yml`](../.github/workflows/codeql.yml),
[`scripts/ci/changed-surfaces.sh`](../scripts/ci/changed-surfaces.sh),
[`deny.toml`](../deny.toml), and [`make/template.mk`](../make/template.mk) own
job names, triggers, tool pins, and gate definitions. Cross-file policies:

- **Merge admission:** Configure GitHub Rulesets to require `required`
  (`ci.yml`) and `codeql-required` (`codeql.yml`) without the strict up-to-date
  rule. Both accept intentionally skipped surface gates (docs-only PRs run no
  Rust jobs); conflicting merges fail post-merge via [Red main](#red-main).
- **PR vs post-merge:** PRs run surface-selected gates (`post_merge=false`);
  `initializer`, `image`, `runtime-progress`, `jobs-reliability`, and CodeQL
  Rust run post-merge on `main`. Schedules, tags, and `workflow_dispatch` select
  every surface.
- **Prerequisites:** Enable Dependabot alerts in derived repos so Dependency
  Review has a graph. GHCR publication (`cd.yml`) requires
  `ENABLE_GHCR_PUBLISH == 'true'`; release tags must equal `v<crate version>`.
- **Source initializer:** `initializer_runtime` runs `make template-init-check`
  planned by
  [`scripts/ci/initializer-matrix.py`](../scripts/ci/initializer-matrix.py)
  from [`scripts/ci/template-init-check.sh`](../scripts/ci/template-init-check.sh),
  and `image-derived` checks release artifact graphs; projected docs alone run
  only `initializer (projections)`. Derived services strip `make/source.mk` and
  source-only runners.

### Profile-conditioned gates

<!-- template:begin grpc:docs-ci-grpc-gates -->
- **gRPC:** `grpc` checks Buf lint/format, codegen drift, and PR-base `FILE`
  compatibility; `security` checks `tools/grpc-codegen/Cargo.toml` against
  `deny.toml` on `cargo_dependencies` or `dependency_policy`.
<!-- template:end grpc:docs-ci-grpc-gates -->
<!-- template:begin postgres:docs-ci-postgres-gates -->
- **PostgreSQL:** `quality` checks forward-only migrations, Squawk lint on
  added files, and embedded rules; `integration` runs `test-integration-db` and
  `make sqlx-check`; `image` runs the `/migrate` rehearsal on migration edits.
<!-- template:end postgres:docs-ci-postgres-gates -->
<!-- template:begin messaging:docs-ci-messaging-gates -->
- **Messaging:** `messaging_integration` runs the Compose NATS suite and
  `make messaging-go-compat`.
<!-- template:end messaging:docs-ci-messaging-gates -->
<!-- template:begin cache:docs-ci-cache-gates -->
- **Cache:** `cache_integration` runs `make test-integration-cache` against
  Compose Valkey.
<!-- template:end cache:docs-ci-cache-gates -->
<!-- template:begin object-storage:docs-ci-object-storage-gates -->
- **Object storage:** `object_storage_integration` runs
  `make test-integration-object-storage` against Compose `versitygw` (never live
  buckets).
<!-- template:end object-storage:docs-ci-object-storage-gates -->
<!-- template:begin outbox:docs-ci-outbox-gates -->
- **Outbox:** Database-and-NATS integration covers commit/rollback, live-key
  conflicts, outage snooze/recovery, uncertain completion, durable consumer
  dedupe, and occupied-webhook isolation.
<!-- template:end outbox:docs-ci-outbox-gates -->
<!-- template:begin outbound-auth:docs-ci-outbound-auth-gates -->
- **Outbound auth:** `oauth_integration` runs `make test-integration-oauth`
  against a digest-pinned throwaway Keycloak container.
<!-- template:end outbound-auth:docs-ci-outbound-auth-gates -->

## Vendored forks

`vendor/<crate>` holds a crates.io archive with a `[patch.crates-io]` patch;
`PATCHES.md` owns provenance, proof, and retirement. Because Dependabot and
`cargo-deny` skip path dependencies, `make dependency-drift` fails on registry
bypasses, base-version OSV advisories, or a missing `Upstream reviewed:` line.
Weekly `dependency-drift` (`--upstream`) opens one issue for post-cooldown
upstream releases and outdated `tools/versions.env` pins (promtool tracks
Prometheus 3.5 LTS).

When `dependency-drift` names a fork:

1. Check upstream changes from base against `PATCHES.md` retirement criteria
   and fixes.
2. **Retirement met:** Remove `vendor/<crate>`, `[patch.crates-io]`, workspace
   exclusions, and profile/classifier/Docker references together, keeping the
   behavior proof at its surviving owner.
3. **Fixes worth taking:** Verify checksum, reapply patch, update `PATCHES.md`
   and standalone lockfile, and rerun `make native-transport-regressions` and
   the profile proof.
4. **Nothing applies:** Update `Upstream reviewed: <version>` with a one-line
   reason. Backport urgent security fixes as exact upstream commits in
   `PATCHES.md`.

## Red main

[`.github/workflows/main-health.yml`](../.github/workflows/main-health.yml)
runs when `ci` or `codeql` finishes on `main`. It checks the latest decided
push runs of both workflows, reruns a first failure once, opens or updates the
`main-red` issue on a second failure, and fast-forwards `green` (closing the
issue) when both pass on the same commit. CD publishes only after a passing `ci`
push run, and template sync reads `green`
([Adopt a committed source](template-sync.md#adopt-a-committed-source)).

While `main-red` is open:

1. **Isolate:** With multiple commits since `green`, push a branch at a suspect
   commit and run `gh workflow run ci.yml --ref <branch>` (selects every
   surface); delete the branch afterwards.
2. **Fix forward:** Merge a PR making the failed jobs pass, reproducing
   post-merge jobs locally via their `make` targets.
3. **Revert fallback:** If `main` stays red two hours after the issue opens, or
   two fix PRs fail, revert via PR. Never revert a migration that shipped in a
   `green` image; fix forward with a new migration.

PRs keep merging while `main` is red; later runs report new failures on the open issue.

## Recovery

- **Failed CI:** Script traps remove ephemeral Compose/Docker containers; no
  external state changes.
- **Failed publication:** Before promotion, deployable tags (`sha-<12>`,
  `main`, `v*`, `latest`) stay untouched, though caches, run tags, or
  signatures may remain. During sequential promotion, check the job summary for
  any unconfirmed tag and verify its registry digest before retrying.
- **Rollback custody:** Retain and verify the accepted immutable digest (or
  Railway deployment identity for [source deployments](railway-deployment-profile.md#rollback))
  with compatible config; startup identity and readiness never prove
  mixed-version SQL or payload compatibility
  ([recovery admission](production-contract.md#operation-and-recovery)).

## Decisions Recorded Here

Recorded with workspace measurements; reopen only with new evidence matching
each reopen condition.

### Tool manifest

| Decision | Rejected | Why |
| --- | --- | --- |
| `tools/versions.env` (`NAME=value`) sourced by Make, shell, and CI | `mise`, `cargo-run-bin`, `tools/Cargo.toml`, `cargo-binstall` | Needs no parser; `mise` reopens if tools outgrow Cargo + Go + Node + Docker |
| Cargo tools built once into `<git-common-dir>/tools` locally; `taiki-e/install-action` in CI | Compiling per CI run | Compiling all four took ~6 min (`cargo-shear` 58 s, `cargo-deny` 121 s, `zizmor` 161 s) |
| SHA256-verified static `cargo-chef` and `cargo-auditable` in Dockerfile | `cargo install` in builder; `cargo-binstall` | Compiling cost 67 s of a 728 s cold 4-vCPU build; `make tools-check` enforces `ARG` parity |
| Redocly via `npx`; `oasdiff`, Gitleaks, `actionlint` via `go run` | Per-binary checksum scripts | `npx`/Go checksum DB verify downloads; `make actionlint` disables host tools for parity |
| `ruff` via `uvx` (`pipx run` in CI), defect rules only (`ruff.toml`) | `requirements.txt`; native binary; formatter | One pinned PyPI linter (`make actionlint` disables pyflakes) |
| ShellCheck, Trivy, lychee as digest-pinned containers | Native binaries | No Cargo/Go module path; Docker is already required for image work |
| Base digests in Dockerfile `FROM` (Dependabot); `tools/versions.env` bumped via `dependency-drift` | Duplicating digests in manifest; Renovate regex | One owner per pin; `make tools-check` verifies `FROM` digests and `rust:<channel>` parity |

### Gates

| Gate | Decision | Rejected | Why |
| --- | --- | --- | --- |
| Policy (`deny.toml`) | `cargo-deny` over Linux gnu targets, `allow-wildcard-paths = true`, `multiple-versions = "warn"`, zero advisory ignores | `cargo-audit`; `cargo-vet` | Excludes macOS-only crates; path deps need `allow-wildcard-paths` |
| Unused deps | `cargo-shear` on workspace and initializer graphs | `cargo-machete`; `cargo-udeps` | Catches orphaned deps in profile graphs (55/68 failed before pruning) |
| Test adequacy | `cargo-mutants` local-only (`make mutants PKG=<crate>`) | PR mutation gate; `mutagen` | 71 mutants in `infra-grpc/router.rs` took 17 min; survivors guide assertions |
| Binary inventory | `cargo auditable build` | Plain `cargo build` | Embeds 178 `pkg:cargo` crates for Trivy inside `/service` |
| Vendored forks | `make dependency-drift` in `security`; weekly `--upstream` issue | `cargo-deny` alone; failing PRs | Path deps bypass `cargo-deny`/Dependabot; upstream releases prompt review |
| Vulnerabilities | `actions/dependency-review-action` (`high`, PRs only) | — | Requires repository dependency graph (Dependabot alerts enabled) |
| Secrets | Gitleaks over range (PR/push) or `HEAD` ancestry (tags, dispatch, schedule, shallow-refused); `.gitleaks.toml` + `.gitleaksignore` | `--all` as gate; baseline file | Keeps full ancestry without masking new commits or worktrees |
| Workflows | `actionlint` (host tools off); `zizmor` (`GH_TOKEN`) | — | Catches cache poisoning and unsafe triggers; `cd.yml` keeps scoped inline ignores |
| Dockerfile | `docker buildx build --check` + `# check=error=true` | `hadolint` | Built into BuildKit; `# check=error=true` fails Railway builds too |
| Static analysis | CodeQL Rust (`build-mode: none`, shipped code via `.github/codeql/rust.yml`) post-merge; Actions on PRs | Default setup; analyzing tests | Excluding tests and `vendor/` examples cut analysis from ~15.5 to ~7 min while keeping SQLx models |
| Image & SBOM | Trivy container (`HIGH,CRITICAL`, `--ignore-unfixed`) + CycloneDX SBOM over the verified local image ID before push | `grype`/`syft`; `cargo-cyclonedx`; `trivy-action` | One container checks CVEs and emits the 194-component SBOM |
| Links | Offline `lychee` container (`--include-fragments`) | `cargo install` lychee; `lychee-action`; `mlc` | Offline fragment checks prevent network flakiness |
| Test runner | `cargo-nextest` + `cargo test --doc`; `cargo test` for DB | Serial `cargo test` | Workspace `cargo test` ran 54 binaries serially in 127 s vs parallel `nextest` |
| Runtime progress | `make runtime-progress-proof` post-merge (100 ms / 100 ms 1-CPU cgroup v2 quota, `worker_threads=1`, 90%+ occupancy + `cpu.stat` throttling + uncooperative negative control) | Unthrottled host test | Forces cheap unary traffic, uploads, stdout backpressure, and CPU load onto one worker so starvation cannot hide behind spare host cores |

### Routing and pipeline invariants

| Decision | Rejected | Why |
| --- | --- | --- |
| Fail-closed `changed-surfaces.sh` (`--union BASE`) + `affected-crates.sh` (`cargo tree -i`, 80% workspace fallback) | `dorny/paths-filter`; manual crate map | Shared classifier drives CI and `make verify`; manifest/toolchain edits fall back to workspace |
| `ALLOW_FULL=1` guards `make check` and `make template-init-check`; `ALLOW_HEAVY=1` guards heavy targets (`make/template.mk`, `make/source.mk`) | Guarding `lint` and `test` | Keeps ordinary `make build`/`test`/`lint` unguarded while blocking accidental heavy runs |
| Short gates (`quality-profiles`, `quality-validation`, `security`, `delivery`, `docs`, `grpc`, `monitoring-rules`, `oauth-integration`, `initializer (projections)`) need `[changes, secrets]` with `!cancelled()` | Depending on `changes` alone; merging short gates | With 20 runners (CodeQL holds 2), queuing long gates first avoids random-order stalls (~15 vs ~20 min simulated) |
| `main` pushes diff from `green`; concurrency replaces pending `main` runs | Diffing from `before` only | Prevents replaced pending runs from dropping commits |
| `Swatinem/rust-cache`: single toolchain via `.github/actions/rust-toolchain`, `save-if` on `main` after full build, `ZSTD_CLEVEL=12`, `[profile.dev]` line-tables-only + dep `debug = false` in `Cargo.toml` | Keeping runner `stable`; partial saves; runner Cargo config | `rust-cache` hashes all toolchains and path lists; first save owns a key until `Cargo.lock` changes, fitting the 10 GB budget |
| `initializer` runs balanced `core`/`full` cache families (max 4 parts; `image-derived` max 2), unified `vendor/` mtimes, one `CARGO_TARGET_DIR` | Per-graph targets; unbounded matrix | Avoids rebuilding `[patch]` crates per clone and leaves ~6 runners for PRs |
| `quality-profiles`, `quality-validation`, and `jobs-reliability` (no fat LTO) split into own jobs | Monolithic `quality` / `integration` | Removes non-artifact checks from critical path |
| `REPOSITORY_CI ?= none` mode in `make/service.mk` | Implicit CI mode | Gives CI-less Railway repos a complete `make verify` (`ci_mode: none`) receipt |

<!-- template:begin postgres:docs-ci-postgres-routing -->
`db_integration` selects real-DB and `sqlx-check` gates; `migrations` selects
static history, Squawk lint, and `/migrate` rehearsal independently of
adapter-only edits.
<!-- template:end postgres:docs-ci-postgres-routing -->

**Cross-file CI notes:** BuildKit `type=gha` silently skips inside `run:` steps
(no `ACTIONS_RUNTIME_TOKEN`), so CI skips BuildKit export while CD exports
`cooked` to `type=registry` in GHCR. Scripts run under `/bin/sh` rather than
zsh, where `status` is read-only.

### Runtime image

Measured on a 4-vCPU `linux/amd64` DigitalOcean host (`Docker 29.1`, empty
cache) across `/service`, `/migrate`, and `/jobs-worker`:

| Variant | Runtime base | Cold build | Source rebuild | Compressed | Trivy (OS) |
| --- | --- | --- | --- | --- | --- |
| glibc (**selected**) | `gcr.io/distroless/cc-debian13:nonroot` | 655 s | 263 s | 30.1 MB | 14 pkgs; 0 HIGH/CRIT (31 unfixed MED/LOW) |
| static musl | `gcr.io/distroless/static-debian13:nonroot` | 666 s (Debian) / 1,173 s (`rust:alpine`) | 266 s | 20.7 MB | 6 pkgs; 0 CVEs |

| CPU/request, musl vs glibc | plain | JSON 1 KiB | JSON 64 KiB | PG read | PG write |
| --- | --- | --- | --- | --- | --- |
| Change | +11% | +33% | +99% | +1% | +2% |

musl's lower RSS (8–15%) and 6 fewer packages do not pay for up to 2x CPU on
JSON (`perf` put 49% of 64 KiB JSON samples in bytewise `memcpy`) plus ~1.8x
slower musl-hosted `rustc` compiles.

Builder and runtime share `debian13` (`trixie` binaries fail on `debian12`).
`cargo-chef` cooks dependencies in a plain layer for registry cache export, and
each binary builds in parallel with `cargo auditable build`. `rust-toolchain.toml`
stays out of `.dockerignore` to prevent per-stage `rustup` downloads. Locked
builds produced byte-identical binaries (`CARGO_INCREMENTAL=0`, `strip = true`,
`SOURCE_DATE_EPOCH`), while APT indexes, `docker/dockerfile:1`, and upstream
downloads remain mutable; rollback custody retains the accepted image digest.
`app.version` comes from Cargo, `app.commit` from `VCS_REF`, and `distroless`
tags are digest-pinned.

<!-- template:begin postgres:docs-ci-migrator-image -->
With PostgreSQL retained, the image builds `/migrate` with embedded migrations;
`runtime-image-check.sh` accepts `RUNTIME_IMAGE_NETWORK` and
`RUNTIME_IMAGE_POSTGRES_DSN` to verify `postgres_pool_opened`.
<!-- template:end postgres:docs-ci-migrator-image -->
<!-- template:begin worker:docs-ci-jobs-worker-image -->
With jobs or messaging retained, the image builds `/jobs-worker`
(`--entrypoint /jobs-worker`); [Container Validation](validation/containers.md)
checks retained/absent entrypoints and startup refusals.
<!-- template:end worker:docs-ci-jobs-worker-image -->

### Publication and deployment

`cd.yml` uses a composite action to keep the Fulcio identity
`…/.github/workflows/cd.yml@<ref>`. `railway.toml` is deprecated in favor of IaC.

| Decision | Rejected | Why |
| --- | --- | --- |
| Ship `.railway/railway.ts` + `.railway/package.json` (`railway` 3.16.0) using partials and linked context | Document-only snippet | Ready for `railway config apply` without template project IDs while preserving operator variables |
| `ARG IMAGE_TESTS=none` skips unit tests unless set (`IMAGE_TESTS=unit` for `REPOSITORY_CI = none`) | Always running `unit`; Railway "Wait for CI" | Avoids +65–70 s/build (+379 s/lockfile) and timing flakes when CI already tests; "Wait for CI" blocks on post-merge jobs |
| Private deps via `vendor-private/` + `.cargo/config.toml` | `ARG` tokens or secret mounts | Railway lacks secret mounts and BuildKit logs `ARG` values |

### Deviations from the Go template

| Go template | Rust template | Why |
| --- | --- | --- |
| `tools/go.mod` + `go tool` | `tools/versions.env` (`cargo install`, `install-action`, `go run`, containers) | No project-local Cargo tool table |
| `govulncheck` + `gosec` | `cargo-deny` + CodeQL Rust | No reachability in `cargo-deny`; CodeQL covers static checks |
| `go mod tidy -diff` / `verify` | `cargo-shear`; `--locked` | Cargo enforces lockfile parity via `--locked` |
| Static Go binary | glibc binary on `distroless/cc-debian13` | Neutral allocator and test parity |
| Go module metadata | `cargo auditable build` | Rust omits dependency metadata by default |
| Go build cache mount | `cargo-chef` layers | Mounts cannot export to registry caches |
| `railway.toml` | `.railway/railway.ts` | Railway deprecated Config as Code |
| Gitleaks baseline | `.gitleaks.toml` + commit-qualified `.gitleaksignore` | Keeps full history scanning intact |
| `gotestsum` | `cargo-nextest` + `cargo test --doc` | Parallel test execution |
| No link checker | Offline `lychee` container (`--include-fragments`) | Prevents broken relative links and `#fragments` |

### Deferred, with the change that reopens each

- **Static musl image:** Musl `memcpy` matching glibc's, re-measured on the
  [Runtime image](#runtime-image) matrix.
- **`mise`:** Tools outgrowing Cargo + Go + Node + Docker.
- **`cargo-vet` / Renovate regex:** Per-service policy in a derived repo.
- **Multi-platform image:** Consumer deploying on another architecture.
- **Second `tower-http` (`axum-tracing-opentelemetry` via `reqwest`):** Warning
  until upstream converges.
- **BuildKit `rewrite-timestamp`:** Consumer requiring identical image digests.
