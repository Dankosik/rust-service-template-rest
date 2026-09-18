# Stage 5 research synthesis: repository documentation

Decisions for roadmap stage 5 (the documentation graph, the link checker,
and the absorption of the stage 2, 3, and 4 research bundles). Versions were
read from crates.io, npm, and GitHub on 2026-09-18. Claims marked *verified*
were executed on this workspace (Docker 29.4.0, macOS arm64); claims marked
*observed* were read in documentation or source.

The Go template supplies the graph: a front door
(`docs/repo-architecture.md`) with global invariants, a source-of-truth
table, and a one-leaf selector; architecture leaves for boundaries, HTTP,
lifecycle, integration, async, and persistence; a placement algorithm; a
command catalog; a production contract that stays unresolved until a service
owns it; and a first-feature guide that a new contributor follows on the
scaffold. It has no link checker; this stage's exit criterion requires one.

## Requirements

From the roadmap stage and the Go reference:

- every relative link in `AGENTS.md` and `docs/` resolves to a file in this
  repository, including `#fragment` targets, and a checker proves it locally
  and in CI without network access;
- the front door selects one leaf per changed pressure and the leaves exist
  for the pressures the current code has (boundaries, HTTP, lifecycle,
  integration); leaves for capabilities that do not exist yet (async,
  persistence) arrive with their stage;
- the placement algorithm is deterministic for crates, modules, files, and
  tests, and matches what the workspace does today;
- the command catalog is the one human-facing explanation of every make
  target and stays in step with `make/template.mk`;
- the production contract is a service-owned template that blocks promotion
  until every field is accepted;
- the first-feature guide is followed once on the scaffold and produces a
  passing `ALLOW_FULL=1 make check`;
- the stage 2, 3, and 4 research bundles move their durable decisions into
  the owning documents and are deleted (`specs/README.md`); Git keeps the
  history.

## Link checker

| Candidate | Latest (date) | Offline relative links | Fragments | Verdict |
| --- | --- | --- | --- | --- |
| lychee, container `lycheeverse/lychee:0.24.2@sha256:e2d19e57…` | 0.24.2 (2026-05-01) | `--offline` checks `file://` targets | `--include-fragments` resolves Markdown heading ids (*verified*: reports `Cannot find fragment` for a wrong anchor and `File not found` for a missing file; 122 links in this repository checked in 48 ms) | **selected**, run through the digest-pinned container like ShellCheck and Trivy, so one pin in `tools/versions.env` serves local and CI runs and no new prerequisite appears |
| lychee via `cargo install` | same | same | same | rejected: `taiki-e/install-action` has no lychee manifest, so CI would compile it (reqwest, tokio) on every run or need a second cache; the container is prebuilt |
| `lycheeverse/lychee-action` | v2 | same | same | rejected: a CI-only mechanism beside a local one, and a second version pin |
| `mlc` (Rust) | 1.2.0 (2025-12-13) | `--offline` (*observed*) | not documented in its README | rejected: fragment checking is the half of the requirement that catches the common breakage (a renamed heading), and it is not documented |
| `markdown-link-check` (npm) | 3.15.0 (2026-07-28) | yes | not documented | rejected: same gap, plus Node.js as a prerequisite for a check that Docker already covers |
| Template-owned Python script | — | trivial | needs GitHub's heading-slug rules | rejected: a solved problem with a maintained tool; the stage 4 ad-hoc check (30 lines, no fragments) was scratch and is not committed |

Mechanics: `make docs-check` runs `lychee --offline --include-fragments
--no-progress` over the tracked and untracked `*.md` files (`git ls-files`,
like `SHELL_FILES`), so `target/` and vendored trees never enter. External
URLs are excluded by `--offline` on purpose: they are not this repository's
contract, and checking them would make the gate flaky. The `documentation`
surface gains its consumer: `make verify` plans `docs-check`, CI runs it in a
`docs` job that installs no toolchain, and `ALLOW_FULL=1 make check` includes
it.

## Documentation graph

| Go document | Rust document | Decision |
| --- | --- | --- |
| `docs/repo-architecture.md` | `docs/repo-architecture.md` | port: invariants restated for crates, source-of-truth table for the current owners, selector rows only for leaves that exist |
| `docs/architecture/boundaries.md` | `docs/architecture/boundaries.md` | port: one row per crate that exists, dependency direction as the crate graph enforces it; profile blocks arrive with their profiles |
| `docs/architecture/http.md` | existing, extended | absorbs the durable HTTP decisions of the stage 2 and 3 bundles (hardened chain order, accept-loop bounds, problem catalog, code-first contract, OpenAPI 3.1, extractors as validator) |
| `docs/architecture/runtime-lifecycle.md` | `docs/architecture/runtime-lifecycle.md` | port with the staged teardown and exit codes from `crates/service/src/bootstrap` |
| `docs/architecture/integration.md` | `docs/architecture/integration.md` | port the neighbour table and the "before enabling a dependency" checklist; the initializer paragraphs wait for their stages |
| `docs/architecture/async.md`, `persistence.md` | deferred | no async or persistence owner exists; stages 8 and 10 |
| `docs/project-structure-and-module-organization.md` | `docs/project-structure-and-module-organization.md` | port with crate and module placement, filename rules, test placement (`#[cfg(test)]` beside the owner, `tests/` for black-box crate tests, a `test/` crate for process and container proof once one exists) |
| `docs/build-test-and-development-commands.md` | `docs/build-test-and-development-commands.md` | port from the real `make help` catalog |
| `docs/production-contract.md` | `docs/production-contract.md` | port; every field unresolved until a service owns it |
| `docs/first-production-feature.md` | `docs/first-production-feature.md` | rewrite for utoipa handlers, feature crates, the router merge in `crates/service/src/api.rs`, the config section files, and the OpenAPI regeneration; followed once on the scaffold (*verified* in this stage) |
| `test/README.md` | deferred | no `test/` crate exists; the roadmap's "do not create a directory before its first real artifact" applies; arrives with the first container-backed proof (stage 8) |
| `specs/README.md` | existing | unchanged |
| `docs/validation-routing.md`, `docs/validation/*`, `docs/ci-cd-production-ready.md`, `docs/railway-deployment-profile.md` | existing since stage 4 | unchanged except links from the front door |
| `CONTRIBUTING.md` | existing | completed against the command catalog |

## Bundle absorption

| Bundle | Durable decisions move to | Removed |
| --- | --- | --- |
| `specs/runtime-core/research/*` | `docs/architecture/runtime-lifecycle.md` (startup order, staged teardown, exit codes, budgets), `docs/architecture/http.md` (hardened chain, accept loop), `docs/configuration-source-policy.md` (already the owner of config decisions), `docs/architecture/boundaries.md` (crate ownership); the roadmap's stage 2 section keeps the deviation summary | the four lane reports and the synthesis |
| `specs/api-contract/research/synthesis.md` | `docs/architecture/http.md` (code-first generation, committed document as authority, OpenAPI 3.1, closed problem schemas, security decisions, compatibility rules) | the synthesis |
| `specs/validation-delivery/research/synthesis.md` | `docs/ci-cd-production-ready.md` (gate decisions, tool table, deviations, gotchas as an appendix), `docs/validation/containers.md` (image measurements), `docs/railway-deployment-profile.md` (already carries the deployment decision) | the synthesis; comments in `Dockerfile`, `deny.toml`, `tools/versions.env`, `ci.yml`, and the scripts point at the owning document |
| `specs/rust-skills/research/synthesis.md` | stays open: stage 7 is in progress | — |

Roadmap stage sections that cite a bundle path cite the owning document
after absorption; Git history keeps the research text.

## Deviations from the Go template

| Go template | Rust template | Why |
| --- | --- | --- |
| No link checker | lychee in a pinned container, offline, with fragments | the stage exit criterion names one; the container matches how ShellCheck and Trivy already run |
| `test/README.md` in the graph | deferred to the first `test/` crate | no directory before its first artifact |
| Placement rules name Go packages, `httpx`, depguard file families | crate graph as the dependency rule, module placement inside a crate, `#[cfg(test)]` and `tests/` | the compiler enforces direction; no depguard |
| First-feature guide edits `service.yaml` then generates Go | the guide edits handlers with `#[utoipa::path]` and regenerates the document | code-first contract (stage 3 decision) |
