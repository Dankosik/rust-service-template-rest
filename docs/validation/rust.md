# Rust Validation

## Bounded local change

Use the local completion criterion in [AGENTS.md](../../AGENTS.md#validation-budget).
Start with the matching build and the tests of the affected crate:

```bash
make build
make test-package PKG=<crate>
```

Include the crates that depend on a changed crate's production code through
the existing planner rather than a second selection mechanism:

```bash
printf '%s\n' crates/health/src/lib.rs | bash scripts/ci/affected-crates.sh
make test-changed PKGS="health infra-http service"
```

`make plan` prints the same selection for the whole worktree without running
it. Confirm the required tests actually ran: a filter that matched nothing, a
skipped process test, or a startup error is not passing evidence.

## Broad Rust change

Run the workspace when `Cargo.toml`, `Cargo.lock`, a crate manifest, or
`rust-toolchain.toml` changes, when a Rust file lives outside `crates/`, or
when the planner falls back (its `fallback_reason` names why):

```bash
make build
make test
```

Feature unification means a manifest change can alter what an untouched crate
compiles, so the affected closure cannot bound it. Stop after the local
criterion and any applicable review; do not append lint, `make verify`, image,
or scan runs for confidence.

## Formatting, lint, and unused dependencies

`make fmt-check` runs over the workspace (sub-second) and `make lint` is clippy
at pedantic with warnings as errors; `make lint-changed PKGS=…` scopes clippy
to the crates the planner selected. A lint or formatting configuration change
(`clippy.toml`, `rustfmt.toml`, workspace lints in `Cargo.toml`) runs the
workspace lint. `make unused-deps` (cargo-shear) fails on a declared dependency
no crate uses; it is part of `ALLOW_FULL=1 make check` and CI runs it whenever
Rust source or a manifest changes, because a removed `use` can orphan a
dependency.

## Full repository

`ALLOW_FULL=1 make check` is the explicit full gate: format, workspace clippy,
workspace tests, unused dependencies, OpenAPI lint, skills, and the
validation-system self-tests, under the shared validation lock. It is what
CI's `quality` job runs piecewise on `main`; it is not a routine follow-up to
every edit, and the guard exists so it is never launched by accident.
