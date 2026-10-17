# CI inventory against `make verify`

Read at `origin/main` c0b4f320 (2026-10-17), source template checkout.
Evidence for [B8](../spec.md#b8-no-ci-verification-mode); line numbers are
from that revision.

## How a step becomes CI-owned

`ci_owned()` (`scripts/ci/verify.sh:1101-1109`) returns true unless
`CI=true`, for a step with `requires_heavy=true` while `ALLOW_HEAVY != 1`, and
for `make template-init-check` while `ALLOW_FULL != 1`. `add_command`
(1117-1121) moves such a step into `ci_owned_displays`; the receipt
(1617-1626) then says `status: partially_verified`, `ci_owned: …`,
`gap_or_next_owner: CI`. `verified` is printed whenever nothing is CI-owned,
including `ALLOW_FULL=1 ALLOW_HEAVY=1`, although CI-only checks without a
plan entry still did not run.

## CI steps and their local plan entry

| CI step (ci.yml) | Selector | Local plan entry today |
| --- | --- | --- |
| `grpc` grpc-check | grpc_schema | `make grpc-check`, heavy |
| quality: fmt, lint/test (PR: affected crates; push: workspace), unused-deps, OpenAPI, migrations, instructions, validation self-tests, readability | their surfaces | planned (fmt-check misses a `cargo_dependencies`-only change) |
| quality: native transport | cargo_dependencies | `make native-transport-regressions`, heavy |
| quality: optional profiling (`cargo check --features hotpath…`, `cargo tree`) | rust_source or cargo_dependencies | **none** |
| quality: OpenAPI compatibility (`make openapi-breaking` against the PR base) | openapi on pull requests | **none** (target exists) |
| security: Dependency Review | PR and deps, tool manifest, workflows or image | **none**; GitHub service |
| security: `make deny` | deps or deny policy | planned |
| security: generator deny (`cargo-deny --manifest-path tools/grpc-codegen/Cargo.toml`) | grpc_schema | **none** |
| security: zizmor | workflows | planned |
| `secrets`: range Gitleaks; history on tag, dispatch, schedule, no base | every event | only when `.gitleaks*` changes |
| `delivery`: actionlint, shellcheck, tools-check, dockerfile-check, publication metadata | their surfaces | planned |
| `image-source`: build, lifecycle or migration rehearsal, Trivy, SBOM | runtime_image (and migrations) | planned, heavy |
| `image-derived`: `template-init-artifacts` | artifact graphs | planned, heavy |
| `runtime-progress` | runtime_progress | planned, heavy |
| integration: DB with `RUN_JOBS_RELIABILITY_REFERENCE=1`, sqlx-check | db_integration | planned, heavy, without the reference variable |
| integration: cache, object storage, JetStream, OAuth | their surfaces | planned, heavy |
| integration: Go wire compatibility | messaging_integration | **none** |
| integration: compile every suite (`--no-run`) | db_integration or a cold main push | none (cache warm-up) |
| `initializer`, `initializer-projections` | initializer surfaces | `make template-init-check` (CI-owned by name), projections |
| `docs` | documentation | planned |
| CodeQL (Rust, Actions) | rust or deps; workflows | **none**; GitHub code scanning |
| cd.yml publish, sign, attest | post-merge, opt-in | not a merge gate |
| messaging-recovery | workflow_dispatch only | not a pull-request gate |

## Ownership in derived services

`.github/` is service-owned: sync never writes it, the initializer prunes
`ci.yml` by profile markers and keeps it. `verify.sh`, `changed-surfaces.sh`
and `make/template.mk` are template-owned and synced; `make/service.mk` holds
repository-owned facts. `docs/ci-cd-production-ready.md` and
`docs/build-test-and-development-commands.md` are service-owned after
initialization.

## Assertions of current behaviour

verify.sh self-tests at 476-480, 501-518, 687-696, 825-839 (exact
`partially_verified`, `ci_owned:` and `gap_or_next_owner: CI` lines);
`docs/validation-routing.md`, `docs/ci-cd-production-ready.md`,
`docs/build-test-and-development-commands.md`, `CONTRIBUTING.md`,
`AGENTS.md` Validation budget.
