# Intent

## Request

Apply every accepted recommendation from the Super Review report for
`crates/infra-jobs` (R-001 through R-033), run the required checks, and land the
result on `main`.

## Outcome

The production source of `crates/infra-jobs` carries all 33 accepted
readability and maintainability recommendations, its in-repo consumers and tests
still compile and pass, and the change is merged to `main` after the repository
gate.

## Scope

- `crates/infra-jobs/src/` and its unit tests.
- Mechanical consumer updates forced by the public renames in R-029 and the
  moved limit definitions in R-030: `crates/jobs-worker`, the `test` crate,
  examples, and documentation that names the changed public paths.
- `.sqlx` metadata and `docs/guides/background-jobs.md` where R-024/R-025 change
  the claim statement or its recorded lease wording.

## Exclusions

- Unresolved observations, which have no implementation instruction: C-05-4,
  C-SR-06-4, C-SR-06-7, C-SR-07-06, C-SR-08-4, C-SR-10-3, C-SR-10-8.
- Rejected candidates: C-05-3 and C-SR-07-05.
- Unrelated work in the source checkout: the modified
  `crates/infra-oauth2-client-credentials/src/lib.rs` and the untracked
  `specs/bitrina-template-sync-20261109/`. This work is outside the review and
  must remain untouched.

## Authority

The request authorizes scoped local edits, non-destructive local validation, a
branch push, a pull request, and the merge into `main`. It does not authorize a
deployment, a release tag, or a remote write outside this repository.

## Success

- R-001 through R-033 are present in the assembled tree.
- The crate and its consumers build; `infra-jobs` tests pass; formatting, lint,
  documentation-link checks, and SQLx metadata checks pass for the changed
  surfaces.
- The pull-request gate is green and the change is merged to `main`.
- No unresolved observation and no rejected candidate is implemented.
