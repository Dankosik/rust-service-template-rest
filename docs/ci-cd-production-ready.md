# CI/CD Production Readiness

What the pipeline proves about a candidate, the decisions behind each gate
([recorded below](#decisions-recorded-here)), and the runbooks for red `main`
and failed runs.

[`.github/workflows/`](../.github/workflows),
[`scripts/verify/surfaces.py`](../scripts/verify/surfaces.py),
[`deny.toml`](../deny.toml), [`tools/versions.env`](../tools/versions.env), and
[`make/template.mk`](../make/template.mk) are the source of truth for job names,
triggers, surface routing, tool pins, and gate commands. [AGENTS.md](../AGENTS.md#validation-budget)
and [Validation Routing](validation-routing.md) own local verification; local
passes never prove CI admission, container behavior, or a release.

## Pipeline policies

- **Merge admission:** Configure GitHub Rulesets to require `required` (`ci.yml`)
  and `codeql-required` (`codeql.yml`) without the strict up-to-date rule. Both
  accept intentionally skipped surface gates; semantic merge conflicts surface
  post-merge via [Red main](#red-main).
- **Repository setup:** Enable Dependabot alerts in derived repositories so
  `dependency-review-action` has a graph, and set real principals in
  `.github/CODEOWNERS` before requiring code-owner review. Image publication in
  `cd.yml` stays inert until repository variable `ENABLE_GHCR_PUBLISH` is
  `'true'`, and release tags must match `v<crate version>`.

## Vendored forks

`vendor/<crate>` holds a crates.io archive wired via `[patch.crates-io]`, with
provenance and retirement criteria in its `PATCHES.md`. Because Dependabot and
`cargo-deny` skip path dependencies, `make dependency-drift` fails on registry
bypasses, base-version OSV advisories or crates.io yanks, generator/runtime
`prost`/`tonic` pin drift, or a missing `Upstream reviewed:` line,
and the weekly `--upstream` workflow opens an issue for post-cooldown upstream
releases and outdated `tools/versions.env` pins, including `GO_VERSION`
(promtool tracks Prometheus 3.5 LTS).

When `dependency-drift` names a fork:

1. Check upstream changes from base against `PATCHES.md` retirement criteria.
2. **Retirement met:** Remove `vendor/<crate>`, `[patch.crates-io]`, workspace
   exclusions, and profile/classifier/Docker references together, keeping the
   behavior proof at its surviving owner.
3. **Fixes worth taking:** Verify the `.crate` checksum, reapply the patch,
   update `PATCHES.md` and the standalone lockfile, and rerun
   `make native-transport-regressions` plus the affected profile proof.
4. **Nothing applies:** Update `Upstream reviewed: <version>` with a one-line
   reason, or backport urgent security fixes as exact upstream commits.

## Red main

[`.github/workflows/main-health.yml`](../.github/workflows/main-health.yml)
evaluates the latest decided `main` push runs of `ci` and `codeql`, reruns a
first failure once, opens or updates the `main-red` issue on a second failure,
and fast-forwards `green` (closing the issue) when both pass on the same commit.
`cd.yml` publishes only after passing exact-SHA `ci` and `CodeQL` runs, and
template sync reads `green` ([Adopt a committed source](template-sync.md#adopt-a-committed-source)).

While `main-red` is open:

1. **Isolate:** With multiple commits since `green`, push a temporary branch at
   a suspect commit and run `gh workflow run ci.yml --ref <branch>` (selects
   every surface); delete the temporary branch afterwards.
2. **Fix forward:** Merge a PR making the failed jobs pass, reproducing
   post-merge gates locally through their `make` targets.
3. **Revert fallback:** If `main` stays red two hours after the issue opens, or
   two fix PRs fail, revert via PR. Never revert a migration that shipped in a
   `green` image; roll forward with a new migration.

## Recovery

- **Failed CI:** Ephemeral Compose/Docker resources clean up via script traps; no
  external state changes.
- **Failed publication:** Deployable tags (`sha-<12>`, `main`, `v*`, `latest`)
  promote only after Trivy, SBOM, keyless Cosign signature, and GitHub
  attestation verification succeed; if sequential tag promotion fails mid-way,
  check the job summary and verify registry digests before retrying.
- **Rollback custody:** Retain and verify the accepted immutable image digest
  (or Railway deployment identity for [source deployments](railway-deployment-profile.md#rollback))
  with compatible config; startup readiness never proves mixed-version SQL or
  payload compatibility ([Production Contract](production-contract.md#operation-and-recovery)).

## Decisions Recorded Here

Reopen only with workspace measurements or new evidence matching the reopen
condition.

### Tooling and gates

| Area | Why this design over alternatives |
| --- | --- |
| `tools/versions.env` + `<git-common-dir>/tools` + `taiki-e/install-action` | Plain `KEY=value` needs no parser (`mise` reopens only if tools outgrow Cargo + Go + Node + Docker); prebuilt binaries avoid ~6 min of CI Cargo tool compilation. |
| Static `cargo-chef` & `cargo-auditable` archives in `Dockerfile` | Avoids 67 s of builder compilation on cold builds while embedding `pkg:cargo` dependency graphs for Trivy/CycloneDX SBOM admission (`grype`/`syft` and `cargo-cyclonedx` rejected to keep one scanner/SBOM container). |
| [`scripts/checks/npx.py`](../scripts/checks/npx.py) (`--ignore-scripts` + 7-day publish-age cooldown), `go run`, `uvx`/`pipx`, digest-pinned containers | Hardens tool downloads through npm publish-age gates (`make tools-check` rejects direct `npx`), the Go checksum DB, and OCI digests without custom per-tool installer scripts (`shellcheck` and `lychee` fall back to local host binaries outside CI when Docker is unavailable); `make actionlint` wires the pinned `SHELLCHECK_IMAGE` container via `stdin` (with local `shellcheck` fallback outside CI) and disables host `pyflakes` for runner parity. |
| `cargo-deny` (Linux targets, `all-features = true`, `[licenses] include-dev = true`, `multiple-versions = "deny"`) + `cargo-shear` + `typos-cli` + CodeQL Rust (`build-mode: none`) | Covers optional `hotpath`/`integration` features and dev licenses across shipped Linux targets, catches orphaned deps across profile projections (`cargo-machete`/`cargo-udeps` rejected) and identifier/contract typos (`typos-cli`), and excludes tests/`vendor/` examples in CodeQL (~15.5 → ~7 min). |
| `cargo-nextest` + `cargo test --doc`; local-only `cargo-mutants` | Parallel test binaries cut serial `cargo test` wall time (127 s → 34–37 s locally); mutation testing (17 min on one module) stays an on-demand adequacy aid rather than a PR gate. |
| Post-merge heavy proofs + `secrets`-gated short jobs + `green` diff base | Keeps PR feedback fast while bounding concurrent runner contention under a 20-runner cap and ensuring superseded `main` runs never skip unproved commits. |
| BuildKit `type=registry` cache in `cd.yml` only; `ARG IMAGE_TESTS=none` default | `type=gha` lacks `ACTIONS_RUNTIME_TOKEN` in `run:` steps and exceeds the 10 GB Actions cache budget alongside `Swatinem/rust-cache`; skipping `IMAGE_TESTS=unit` when CI runs saves 65–70 s/build (enable `IMAGE_TESTS=unit` when `REPOSITORY_CI = none`). |

### Runtime image

Measured on a 4-vCPU `linux/amd64` DigitalOcean host (`Docker 29.1`, empty cache):

| Variant | Runtime base | Cold build | Source rebuild | Compressed | JSON 1 KiB / 64 KiB CPU vs glibc |
| --- | --- | --- | --- | --- | --- |
| glibc (**selected**) | `gcr.io/distroless/cc-debian13:nonroot` | 655 s | 263 s | 30.1 MB | baseline |
| static musl (rejected) | `gcr.io/distroless/static-debian13:nonroot` | 666 s / 1,173 s (`rust:alpine`) | 266 s | 20.7 MB | +33% / +99% (`memcpy`) |

musl's smaller image and 8–15% lower RSS do not repay up to 2x JSON CPU and ~1.8x slower musl-hosted `rustc` builds; reopen static musl only if musl `memcpy` matches glibc on this workload. Builder and runtime share Debian 13 (`trixie`), `.dockerignore` excludes `rust-toolchain.toml` so `Dockerfile` owns builder `rustup` state, and `cd.yml` uses a local composite action (rather than a reusable workflow) so keyless Fulcio certificates retain `.../.github/workflows/cd.yml@<ref>` as their signing identity.

### Profile-guided build and compiler flags

Measured on DigitalOcean c-4 hosts (`linux/amd64`, Debian 13, service on two CPUs with two workers, five interleaved 15 s rounds per variant); CPU per request against the default release build on the same host:

| Option | Plain | JSON 1 KiB | JSON 64 KiB | Cost | Decision |
| --- | ---: | ---: | ---: | --- | --- |
| PGO, profile of the measured routes | −30% | −31% | −33% | builds the service twice | opt-in `PGO=train` |
| PGO, profile of `build/docker/pgo-train.sh` as shipped (liveness, readiness, an unrouted path, metrics), through the Dockerfile | −27% | −20% | −2% | about 17 min more per image build on 4 vCPUs (760 s counted build and run, 533 s profiled build) | the shipped training set |
| `-C target-cpu=x86-64-v3` | −0.7% | −1.0% | −0.2% | excludes hosts older than Haswell | rejected: within noise |
| `-C force-frame-pointers=yes`, unstripped | +0.5% | −0.1% | +1.5% | binary 20.9 → 31.5 MB | not adopted |

The route-specific profile also cut p99 latency by 28–32%, and routes absent from it still gained 19–30%. The shipped training set carries no payload route, so a derived service adds the dependency-free routes that carry its traffic to `build/docker/pgo-train.sh`; the profile is merged sparse, and the untrained 64 KiB path did not regress. `PGO` stays `none` by default because it compiles the service's whole dependency graph twice more on every image build, outside the cooked layer, and makes the binary differ between builds; only the service binary is profiled, since the worker and the migrator cannot run without their dependencies. The image label `service.build.pgo` records which build produced it; nothing checks that the profile was applied, so the Dockerfile compiles both builds through one command to keep their symbol names equal. No CI job builds with `PGO=train`: delete the option instead of repairing it when a toolchain or `cargo-auditable` upgrade breaks it while no derived service has it enabled. Reopen `x86-64-v3` for a workload dominated by in-process computation, and frame pointers with symbols when the deployment platform allows `perf` or eBPF profiling of the container.

### Reopen conditions for deferred options

- **Multi-platform image or BuildKit `rewrite-timestamp`:** Consumer deploying on non-native architecture or requiring reproducible OCI layer timestamps beyond binary-level determinism.
- **`cargo-vet` or Renovate regex managers:** Derived service adopting organization-specific supply-chain audit or custom pin automation.
