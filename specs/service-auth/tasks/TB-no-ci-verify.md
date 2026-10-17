# TB — no-CI verification mode

Outcome:
With `REPOSITORY_CI = none`, `make verify` runs every step the template
delegates to CI that has a local command, names the rest with their residual,
and records `ci_mode: none` with status `verified` or a failure, never
`partially_verified`; with `github-actions` nothing changes.

Consumes:
- [spec B8](../spec.md#b8-no-ci-verification-mode).
- [CI inventory](../research/ci-inventory.md).

Provides:
- `REPOSITORY_CI` in `make/service.mk`; mode handling in `scripts/ci/verify.sh`; make targets for CI-only steps (profiling configurations, generator dependency policy, Go wire compatibility, compile-only integration suites); verify self-tests for both modes; documentation in `docs/validation-routing.md`, `docs/build-test-and-development-commands.md`, `docs/ci-cd-production-ready.md`, `CONTRIBUTING.md`.

Boundary:
The plan, guards and receipt only. It does not change CI, the classifier's
surfaces, or what each step proves. CodeQL, Dependency Review, publication,
scheduled and dispatch-only workflows stay outside with named residuals.

Mutable owners:
- Verification route (`scripts/ci/verify.sh`, `make/template.mk`, `make/service.mk`).
- Validation and contribution documentation.

Exclusive locks:
- `scripts/ci/verify.sh` receipt format.

Final validation:
- Claim: in no-CI mode a receipt is never `partially_verified`; heavy steps and CI-only steps are planned and admitted by their guards; default mode output is unchanged.
- Checks: `make verify-check` (self-tests); `make changed-surfaces-check`; shellcheck of changed scripts; a real no-CI `make verify` on this branch, run locally.
- Observable: self-test output and the receipt of that run.

Reopen if:
A CI step has neither a local command nor an accepted residual.
