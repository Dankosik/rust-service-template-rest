# Contributing

Keep changes deterministic, reviewable, and owned by the narrowest crate or
repository surface that can prove them.

## Prerequisites

- [rustup](https://rustup.rs). The pinned toolchain in `rust-toolchain.toml`
  installs on first use, or explicitly with `rustup toolchain install`.
- GNU Make.

Nothing else is required for the current scaffold. Later stages add Docker
for integration proof and a small set of pinned Cargo tools; the roadmap
names them when they land.

## Validate a change

Ordinary local development finishes after the agreed behavior is implemented,
a matching build and relevant tests pass, and known in-scope defects are fixed:

```bash
make build
make test-package PKG=<crate>      # one crate
make test                          # several crates or a manifest change
```

`make check` (`fmt-check`, `lint`, `test`) is the explicit full-repository
gate and what CI runs; it is not a routine follow-up to every edit. Format
with `make fmt`. Every Cargo command runs with `--locked`: if a change needs a
lockfile update, make it deliberately and commit `Cargo.lock` with the change.

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
  organization policy; require the `required` check. The repository does not
  mutate its own GitHub settings.
- Treat `.github/workflows/ci.yml` as the source of truth for current check
  names instead of copying a list into scripts or docs.

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
