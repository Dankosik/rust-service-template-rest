# Validation Routing

Use this router for explicit verification leaves beyond the ordinary local
budget. [AGENTS.md](../AGENTS.md#validation-budget) owns ordinary local
completion (including [Instruction Validation](validation/instructions.md) on
`make check-instructions` and offline link checks on `make docs-check`);
[`make/template.mk`](../make/template.mk) owns command composition (`make help`),
and [`scripts/ci/changed-surfaces.sh`](../scripts/ci/changed-surfaces.sh) is the
classifier shared by CI and `make verify`.

## Explicit verification and external gates

Load a leaf only for a matching verification requirement or diagnostic.

| Required claim | Load | Existing command |
| --- | --- | --- |
| OpenAPI drift or compatibility | [Generated Contracts](validation/generated.md) | `make openapi-check`, `make openapi-breaking BASE_OPENAPI=<file>` |
| Advisories, licenses, unused deps, secrets, workflows, image CVEs | [Security](validation/security.md) | matching security target |
| Clone admission or crate dependency direction | [`quality/architecture.json`](../quality/architecture.json), [`.jscpd.json`](../.jscpd.json) | `make duplication-check`, `make architecture-check`, `make quality-check-self-test`; source-only `make template-quality-projections` |
| CI workflows, shell/Python scripts, tool pins, Dockerfile, publication | [Delivery](validation/delivery.md) | matching delivery target |
| Runtime image build, lifecycle, scan, or SBOM | [Containers](validation/containers.md) | `ALLOW_HEAVY=1 make runtime-image-build` and required scenario |
| Real PostgreSQL transactions, locks, commit outcome, readiness, or migrations | [PostgreSQL](validation/postgres.md) | `ALLOW_HEAVY=1 make test-integration-db`, `make migration-check`, `ALLOW_HEAVY=1 make migration-validate` |

CI and publication gates keep their own admission scope
([CI/CD Production Readiness](ci-cd-production-ready.md)).

## `make plan` and `make verify`

`make plan` runs [`scripts/ci/verify.sh`](../scripts/ci/verify.sh) `--plan` to
print changed files, surfaces, planned commands, CI-owned steps, and no-op
surfaces. `make verify` runs local steps under the Git-common validation lock
([`scripts/ci/validation-lock.py`](../scripts/ci/validation-lock.py)) and
records attempts and reusable receipts under `<git-common-dir>/codex/verify`
(`VERIFY_FORCE=1` bypasses reuse; failures never auto-clean artifacts or caches).

- **CI mode:** By default (`github-actions`), heavy steps and the source
  initializer matrix stay CI-owned (`status: partially_verified`). Setting
  `REPOSITORY_CI ?= none` in `make/service.mk` runs all local/heavy/CI-only
  checks under `ALLOW_HEAVY=1` and `ALLOW_FULL=1` (`ci_mode: none`), listing
  GitHub-hosted gaps under `not_applicable`.
- **Rust selection:**
  [`scripts/ci/affected-crates.sh`](../scripts/ci/affected-crates.sh) selects
  changed crates and reverse dependents (`cargo tree -i`), falling back to the
  workspace for manifests, lockfiles, toolchains, non-`crates/` Rust, or >=80%
  closures.
- **Readability surfaces:** `duplication` and `architecture` require Python,
  Cargo, and `npx`.

`ALLOW_FULL=1` guards `make check` and `make template-init-check`;
`ALLOW_HEAVY=1` guards the container-backed, history-wide, and heavy integration
targets in [`make/template.mk`](../make/template.mk) and
[`make/source.mk`](../make/source.mk). CI sets `CI=true`; never set it locally.
