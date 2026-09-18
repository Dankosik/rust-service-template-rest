# Contributing

Keep changes deterministic, reviewable, and owned by the narrowest crate or
repository surface that can prove them.

## Prerequisites

- [rustup](https://rustup.rs). The pinned toolchain in `rust-toolchain.toml`
  installs on first use, or explicitly with `rustup toolchain install`.
- GNU Make.
- Node.js, for `make openapi-lint` (Redocly CLI through `npx`); it is part
  of `make check`.
- Go, for `make openapi-breaking` (oasdiff), `make secret-scan` (Gitleaks),
  and `make actionlint`, all through `go run`; CI runs them, locally they are
  optional.
- Docker with BuildKit, for `make shellcheck` (pinned container),
  `make dockerfile-check`, and the image targets (`runtime-image-build`,
  `runtime-image-check`, `container-security`, behind `ALLOW_HEAVY=1`);
  later stages add integration proof.

Every tool version is pinned once in `tools/versions.env`; `make` and CI read
the same file, and `make tools-check` proves the pins resolve. The Cargo tools
(`cargo-deny`, `cargo-shear`, `zizmor`) build from crates.io into the Git
common directory the first time a target needs them, once per version.

## Validate a change

Ordinary local development finishes after the agreed behavior is implemented,
a matching build and relevant tests pass, and known in-scope defects are fixed:

```bash
make build
make test-package PKG=<crate>      # one crate
make test-changed PKGS="a b"       # the crates scripts/ci/affected-crates.sh prints
make test                          # several crates or a manifest change
make plan                          # the route the changed surfaces select
make verify                        # run that route and record a receipt
```

`make plan` classifies the changed paths (`scripts/ci/changed-surfaces.sh`,
fail-closed on an unknown path) and prints the make targets that prove them;
`make verify` runs them under the shared validation lock and writes a receipt
under `<git-common-dir>/codex/verify` keyed by the changed files, the plan,
and the environment. CI runs the same classifier.

`ALLOW_FULL=1 make check` (`fmt-check`, `lint`, `test`, `unused-deps`,
`openapi-lint`, `check-skills`, and the validation-system self-tests) is the
explicit full-repository gate; it is not a routine follow-up to every edit,
and the guard exists so it is never launched by accident. `ALLOW_HEAVY=1`
guards the history-wide and container-backed commands the same way; CI sets
`CI=true`, which satisfies both. Format with `make fmt`. Every Cargo command
runs with `--locked`: if a change needs a lockfile update, make it
deliberately and commit `Cargo.lock` with the change. `make deny`
(advisories, licenses, bans, sources) and `make secret-scan` are the
dependency and secret gates CI runs; run them locally when a change touches
`Cargo.toml`, `Cargo.lock`, `deny.toml`, or adds anything that could look
like a credential.

A change to an HTTP operation is made in the handler's `#[utoipa::path]`
attributes and schema derives, then `make openapi-generate` rewrites
`api/openapi/service.yaml`; commit the YAML with the change and review its
diff as the contract change. `make test` fails on a stale copy.
[HTTP Architecture](docs/architecture/http.md) has the full workflow.

[AGENTS.md](AGENTS.md#validation-budget) owns the local stop rule. Missing
optional infrastructure is a gap to disclose, not a blocker to repair; a known
real defect still requires correction.

## Pull requests and repository policy

- Keep pull-request scope focused and reversible; one roadmap stage or one
  profile per series.
- Include exact validation evidence and any unverified remainder.
- Update `docs/roadmap.md` when a stage completes or its scope changes, and
  update other docs with behavior, contract, CI, or operational changes.
- Configure required reviews and status checks with GitHub Rulesets or
  organization policy; require the `required` check from `ci.yml` and
  `codeql-required` from `codeql.yml`. Both accept a gate the changed
  surfaces did not select and fail on any failed or cancelled one, so a
  docs-only pull request passes without running a Rust job. The repository
  does not mutate its own GitHub settings.
- Dependency Review in the `security` job needs the repository's dependency
  graph; enable Dependabot alerts (Settings → Code security, or
  `gh api -X PUT repos/<owner>/<repo>/vulnerability-alerts`) once in a
  derived repository, or the job fails on its first pull request.
- Treat `.github/workflows/ci.yml` as the source of truth for current check
  names instead of copying a list into scripts or docs. Jobs are selected by
  `scripts/ci/changed-surfaces.sh`, the classifier `make plan` uses; a new
  path family joins the classifier with its gate, never as an unclassified
  path (the classifier fails closed).

## Code and workspace

- One crate per ownership boundary under `crates/`; the crate graph is the
  dependency-direction rule. `crates/service` composes; `crates/infra-*`
  adapt; `crates/<feature>` will own business behavior.
- Declare dependency versions once in `[workspace.dependencies]` with
  `default-features = false`; enable features per crate.
- Lints are workspace-level in `Cargo.toml`; do not add per-crate `allow`
  attributes for a lint the workspace enables without a comment stating the
  reason at the site.
- Prefer explicit Rust and existing repository seams over new framework
  layers. Business logic never depends on axum, Tokio I/O types, or a database
  driver.
- Tests live beside their owner; bound every wait; join every spawned task.

## Security and ownership

Do not open public issues for undisclosed vulnerabilities; follow
[SECURITY.md](SECURITY.md). Before enabling required code-owner reviews in a
derived repository, confirm `.github/CODEOWNERS` names real users or teams
with access.
