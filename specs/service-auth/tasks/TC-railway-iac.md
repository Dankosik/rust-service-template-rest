# TC — Railway IaC and secret-free build

Outcome:
A repository initialized from the template carries `.railway/railway.ts`
(partial, service, build and deploy policy, declared variables, preserved
secrets) with a pinned SDK, its image builds without any credential, and with
`IMAGE_TESTS=unit` the build fails on a failing unit test.

Consumes:
- [spec B9](../spec.md#b9-railway-iac-and-build).
- [Railway IaC research](../research/railway-iac.md).
- TA's service-auth variables and marker profile.

Provides:
- `.railway/railway.ts`, `.railway/package.json`, `.railway/package-lock.json`; initializer identity substitution and markers for them.
- `scripts/ci/image-inputs-check.py` reading the IaC file when present; `changed-surfaces.sh` classification of `.railway/*`.
- Dockerfile `IMAGE_TESTS` stage and context admissions; watch patterns.
- `docs/railway-deployment-profile.md` and `docs/ci-cd-production-ready.md` updates, including the reopened deferral.

Boundary:
Repository files and checks only: no Railway project, link, variable or
deployment. PR #274 is not merged here; its private-dependency method is
referenced, and the build's two rules are stated in the profile document.

Mutable owners:
- `.railway/`, `build/docker/Dockerfile`, `.dockerignore`, image-input checker and its test, surface classifier, Railway and CI/CD documentation.

Exclusive locks:
- `build/docker/Dockerfile`, `.dockerignore`.

Final validation:
- Claim: the IaC file evaluates with the pinned SDK and passes the policy check; an image builds with no build secret; with `IMAGE_TESTS=unit` unit tests run inside the build.
- Checks: `make docs-check`; `make dockerfile-check`; `scripts/tests/image-inputs-check.py`; a local evaluation of `railway.ts` with the pinned SDK; `make runtime-image-build` with `IMAGE_TESTS=unit` when the workstation's capacity admits it, otherwise an explicit gap.
- Observable: check output, evaluated IaC JSON, image build log.

Reopen if:
Railway rejects a declared field, or the unit tests need inputs the image context cannot admit.
