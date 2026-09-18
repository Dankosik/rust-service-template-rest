# CI/CD Production Readiness

What the pipeline proves about a candidate, in the order a change meets it.
Decisions and measurements: `specs/validation-delivery/research/synthesis.md`.

## Local leaves

Ordinary local completion is a matching build and relevant tests under
[AGENTS.md](../AGENTS.md#validation-budget); [Validation Routing](validation-routing.md)
selects anything beyond that. `make verify` runs the surface-aware route and
records a receipt; `ALLOW_FULL=1 make check` is the explicit full gate. Local
completion does not assert platform admission, runtime behavior, or a
release; an explicit request for green CI or a release retains that outcome
until the real result exists.

## Pull-request and push CI

[ci.yml](../.github/workflows/ci.yml) classifies the exact diff once
(`scripts/ci/changed-surfaces.sh`, the classifier `make verify` uses; a pull
request that changes the classifier is judged by both the base and the head
version) and starts only the jobs its surfaces select:

| Job | Selected by | Proves |
| --- | --- | --- |
| `quality` | Rust source, manifests, lint config, OpenAPI, instructions, validation system | format; on pull requests clippy and tests of the affected crates and their dependents, on `main` and on any manifest change the workspace; cargo-shear; Redocly lint plus the drift and contract tests; oasdiff against the base; skills; the validation scripts' self-tests |
| `security` | manifests, `deny.toml`, workflows; tool manifest and image on pull requests | cargo-deny (advisories, licenses, bans, sources); Dependency Review, fail on high, pull requests only; zizmor with the online audits |
| `secrets` | every event except a schedule without a policy change | Gitleaks over the commits since the base; the whole history on tags, manual runs, and a push without a readable base |
| `delivery` | shell, workflows, tool manifest, image, publication metadata | actionlint; ShellCheck over the changed scripts; `tools-check`; BuildKit Dockerfile checks; the publication metadata self-test |
| `image` | `build/docker/*`, `.dockerignore`, the image scripts | one `service:ci` image: build (layers restored from the Actions cache, written only by pushes to `main` and schedules), hardened lifecycle check asserting `app.commit`, Trivy |
| `required` | always | fails when any job failed or was cancelled; accepts skipped jobs |

A weekly schedule and manual dispatch select every surface, because advisory
databases move without a commit. Tags select every surface too. A docs-only
pull request runs `changes`, `secrets`, and `required` and no Rust job
(verified on [#11](https://github.com/Dankosik/rust-service-template-rest/pull/11)).
Every action is pinned by commit SHA with its version beside it; tool
versions come from `tools/versions.env` through `GITHUB_ENV` and
`taiki-e/install-action`.

[codeql.yml](../.github/workflows/codeql.yml) runs CodeQL for Rust
(`build-mode: none`) when Rust source or manifests change and for Actions
when workflows change, with `security-events: write` scoped to the analyze
jobs. `codeql-required` accepts skipped analyses and rejects failed ones.

GitHub Rulesets or organization policy own merge admission: require
`required` and `codeql-required`. The repository does not rewrite its own
protection settings. Dependency Review needs the repository dependency graph
(Dependabot alerts enabled), an operator step for a derived repository.

## Generated contracts

The OpenAPI document is generated from the handlers and committed; the drift
test in `make test` refuses a stale copy, Redocly lints it, and pull requests
compare it with the base through oasdiff. Generated output is never edited by
hand ([Generated Contracts](validation/generated.md)).

## Secrets and dependencies

`.gitleaks.toml` carries the default rules and a `target/` allowlist; there
is no baseline file because the history is clean. `deny.toml` restricts the
graph to the two Linux gnu targets and lists the one advisory ignore with its
reopen condition. Duplicate crate versions are warnings, not failures.
Dependabot updates Cargo dependencies, GitHub Actions in workflows and in the
composite publication action, and the Dockerfile `FROM` digests; the tool
manifest is bumped by hand.

## Publication

[cd.yml](../.github/workflows/cd.yml) has one job, gated by the repository
variable `ENABLE_GHCR_PUBLISH == 'true'`. Main publication consumes a
successful same-repository push run of `ci`, waits for the exact-SHA CodeQL
run, and checks out that SHA; a `v*` tag waits for its own `ci` and CodeQL
runs. The shared [publish-image action](../.github/actions/publish-image/action.yml):

1. names the candidate (`scripts/ci/publish-image-metadata.sh`); a release
   tag must equal `v<crate version>`;
2. builds one run-scoped candidate for the exact commit with the same make
   target CI uses;
3. repeats the hardened lifecycle check and the Trivy scan;
4. writes a CycloneDX SBOM from the pinned Trivy container;
5. pushes the candidate and resolves its digest;
6. signs the digest keyless with cosign and attests provenance and SBOM;
7. verifies the signature and both attestations back out of GHCR;
8. records the digest, uploads the SBOM artifact;
9. promotes `sha-<12>` + `main` or `v*` + `latest`, reading each tag's digest
   back; a partial failure records the promoted and failed tags.

Public tags never move before verification. Consumers verify with
`cosign verify --certificate-identity
https://github.com/<owner>/<repo>/.github/workflows/cd.yml@refs/heads/main`
(or `@refs/tags/v…`) and `gh attestation verify oci://<image>@<digest> --repo
<owner>/<repo>`. Nothing is published by this template repository; stage 12
exercises the path once.

## Recovery

- Failed CI changes no external state; each job's containers are removed by
  the scripts' traps.
- A failed publication never promotes public tags; the run-scoped candidate
  tag is the only pushed reference.
- Rollback resolves a previously verified digest rather than rebuilding it.
