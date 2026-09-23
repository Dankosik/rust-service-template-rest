# Validation Routing

Use this router for mixed surfaces or a verification claim beyond the
ordinary local budget. [AGENTS.md](../AGENTS.md#validation-budget) owns
ordinary local completion. This router selects existing commands; a file path,
a domain label, or an available command does not create a local acceptance
gate. `make/template.mk` owns command composition
([Commands](build-test-and-development-commands.md) explains every target),
and `scripts/ci/changed-surfaces.sh` is the one classifier CI and
`make verify` share.

## Ordinary local completion

| Changed surface | Load | Local validation |
| --- | --- | --- |
| Rust behavior, tests, or crate manifests | [Rust](validation/rust.md) | matching build and relevant tests |
| Agent instructions, roles, skills, or their carriers | [Instructions](validation/instructions.md) | static consistency review and `make check-instructions` |
| Documentation | — | `make docs-check`: every relative link and `#fragment` resolves (lychee, offline, pinned container) |

Stop when that boundary passes. Unrun optional image, security, or
publication checks do not block local completion; a missing required build or
test execution is not a pass.

## Explicit verification and external gates

Load a leaf only for a matching verification requirement or diagnostic
question. Regenerating a changed contract is implementation work; this table
does not make every available check mandatory locally.

| Required claim | Load | Existing command |
| --- | --- | --- |
| OpenAPI document drift or compatibility | [Generated Contracts](validation/generated.md) | `make openapi-check`, `make openapi-breaking BASE_OPENAPI=<file>` |
| Dependency advisories, licenses, unused dependencies, secrets, workflow security, image vulnerabilities | [Security](validation/security.md) | matching security target |
| CI workflows, shell scripts, tool pins, Dockerfile lint, publication naming | [Delivery](validation/delivery.md) | matching delivery leaf |
| Runtime image build, lifecycle, scan, or SBOM | [Containers](validation/containers.md) | `ALLOW_HEAVY=1 make runtime-image-build` and the required scenario |
| Observed transaction, locking, commit-outcome, readiness, or migration behavior on a real PostgreSQL; the image's migration rehearsal | [PostgreSQL](validation/postgres.md) | `ALLOW_HEAVY=1 make test-integration-db`, `make migration-check`, `ALLOW_HEAVY=1 make migration-validate` |

Existing CI and publication gates keep their own admission scope
([CI/CD Production Readiness](ci-cd-production-ready.md)). Do not change the
classifier or reproduce a gate locally merely to finish development. An
explicit request for green CI, a runtime scenario, or a release still requires
that result; local completion alone does not complete it.

## `make plan` and `make verify`

`make plan` prints the route the changed surfaces select: the files, every
surface's verdict, the commands with their reason, cost class, and whether
they need Docker, the CI-owned steps, and the surfaces with nothing to run. It
is a diagnosis, not a gate, and does not authorize the plan.

Heavy steps (the image gates, the database-backed proof, the migration
rehearsal) and the source initializer matrix are CI-owned: CI runs them on
every surface that selects them, so `make verify` names them instead of
occupying the workstation. `ALLOW_HEAVY=1` and `ALLOW_FULL=1` keep them local.

`make verify` runs the route's local steps under the Git-common validation lock
and records them. Before executing it checks the binaries the plan needs, and
Docker when a local step is container-backed. Each run writes an attempt
record under `<git-common-dir>/codex/verify` with the plan, candidate
fingerprint, environment, and per-step state; only a complete passing run
writes a receipt, keyed by the changed files' content and modes, HEAD, the
plan, and the environment. An identical rerun reuses that receipt
(`VERIFY_FORCE=1` bypasses it). A step that changes the candidate invalidates
the attempt; a failed or interrupted attempt keeps its evidence and grants
nothing. The receipt names the commands, inputs, environment, duration, and
the surfaces that had no executable check. While CI-owned steps remain it
records `status: partially_verified`, lists them under `ci_owned`, and names
CI as the next owner; a route with only CI-owned steps runs nothing locally.

On pull requests the same classifier selects the affected crates through
`scripts/ci/affected-crates.sh`: a changed crate plus every workspace crate
that depends on it (`cargo tree -i` over normal, build, and dev edges), with a
crate's `tests/` directory reselecting only that crate. A manifest, lockfile,
or toolchain change, a Rust file outside `crates/`, or a closure covering 80%
of the workspace runs the workspace instead. `make lint-changed PKGS=…` and
`make test-changed PKGS=…` consume the list it prints.

`ALLOW_FULL=1 make check` remains the explicit deterministic full-repository
gate, never a default follow-up. `ALLOW_HEAVY=1` guards the image targets,
the database-backed proof, the migration rehearsal, and the history-wide
secret scan. CI sets `CI=true`, which satisfies both; do not set it locally
to impersonate CI.
