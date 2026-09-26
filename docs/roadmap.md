# Roadmap

This repository ports the ideas, instructions, and delivery discipline of
[go-service-template-rest](https://github.com/Dankosik/go-service-template-rest)
to Rust on Tokio and axum. It is a port of *decisions*, not of code: every Go
mechanism is re-derived from what the Rust ecosystem and type system already
provide, and dropped when Rust makes it unnecessary.

The work is split into stages that each land as one or a few reviewable pull
requests. A stage is done when its exit criteria hold on `main`; later stages
may refine earlier ones, but never leave a stage half-wired. Partial capability
is not a supported template state.

## Status

| Stage | Name | State |
| --- | --- | --- |
| 1 | Bootstrap: repository, workspace, runnable health-only service | done |
| 2 | Runtime core: configuration, logging, telemetry, hardened HTTP | done |
| 3 | OpenAPI-first contract and generated bindings | done |
| 4 | Validation routing and delivery: CI surfaces, security gates, image, publication | done |
| 5 | Repository documentation: architecture, placement, commands, production contract | done |
| 6 | Agent harness and spec-first workflow | done |
| 7 | Rust backend skills and universal disciplines | in progress: core set done, capability skills arrive with their stages |
| 8 | PostgreSQL profile | done |
| 9 | Template initializer, profiles, and template sync | done on merge after required CI |
| 10 | Optional capability profiles | 10.1, 10.2, 10.3, and 10.4 merged; remaining profiles planned |
| 11 | Benchmarking and performance evidence | planned |
| 12 | First release and derived-repository verification | planned |

Stages 2 and 3 are independent of each other; 4 and 5 depend on both. Stage 6
depends on 5. Stage 7's core set was pulled forward after stage 2 so the
remaining stages are implemented through skills; its capability skills and
the Claude/Qwen discovery views follow their stages. Stage 9 depends on 4, 5, and 8. Stage 10 items are
independent of each other and each depends on 9 for its profile marker,
except that 10.3 also depends on 10.1 for an authentication engine and on 8
for PostgreSQL, and 10.4 also depends on 8 for PostgreSQL.

## Decisions fixed in stage 1

| Decision | Choice | Why |
| --- | --- | --- |
| Async runtime and HTTP framework | Tokio and axum | axum is the Tokio project's HTTP framework; the request named Tokio. Alternatives (hyper directly, actix-web) either duplicate what axum does over hyper or leave the Tokio ecosystem. |
| Code organization | Cargo workspace with one crate per ownership boundary under `crates/` | The crate graph enforces dependency direction at compile time, which the Go template needs `depguard` for. |
| Ownership mapping | `crates/service` is the composition root (Go `cmd/service`); `crates/infra-<provider>` are adapters (Go `internal/infra/<provider>`); `crates/<feature>` will hold business behavior (Go `internal/<feature>`) | Names carry the same boundary meaning as the Go template so the ported instructions stay recognizable. |
| Toolchain | Exact stable version pinned in `rust-toolchain.toml`, edition 2024, `rust-version` in the workspace | Same policy as the Go template's pinned `go` directive: every checkout and CI runner builds with one compiler. |
| Portable command surface | `Makefile` includes `make/template.mk`; services add `make/service.mk` | Keeps the Go template's split between portable and service-owned commands, which the template-sync stage relies on. |
| Lints | Workspace-level `clippy::pedantic` at `warn`, `unsafe_code = "forbid"`, `-D warnings` in `make lint` and CI | Safe defaults without making a work-in-progress build fail locally. |
| Lockfile | `Cargo.lock` committed; every Cargo invocation uses `--locked` | A binary template must build the same dependency set everywhere, like `GOFLAGS=-mod=readonly`. |
| License | MIT, same as the source template | |

## Concept map

| Go template | Rust template | Notes |
| --- | --- | --- |
| `go.mod` module path; `make template-init MODULE=...` | Workspace package names; initializer rewrites the service crate name, binary name, and owners | No module path to rewrite; less initializer surface. |
| `tools/go.mod` pinned developer tools | `tools/versions.env`: one `NAME=value` pin per tool, read by `make`, shell, and CI; Cargo tools (`cargo-deny`, `cargo-shear`, `zizmor`) built once per version into the Git common directory locally, prebuilt in CI through `taiki-e/install-action`; Go tools through `go run`; ShellCheck and Trivy as digest-pinned containers | Cargo has no project-local tool table; `make tools-check` proves the pins resolve. `cargo-nextest` reopens at stage 8, `cargo-audit` and `cargo-machete` were rejected ([CI/CD Production Readiness](ci-cd-production-ready.md#decisions-recorded-here)). |
| `cmd/service/main.go` → `bootstrap.Run` | `crates/service/src/main.rs` → `bootstrap::run` | Runtime construction, signals, and drain live in `bootstrap`; `main` only maps the result to an exit code. |
| `internal/<feature>` | `crates/<feature>` | Not created until the first feature exists. |
| `internal/infra/http` (`chi`, `net/http` server) | `crates/infra-http` (axum `Router`, `tower-http` layers, hand-rolled hyper accept loop) | Middleware order is the route tree's observable semantics in both. `axum::serve` exposes no connection limits and sets no timer, so the accept loop is template-owned. |
| `internal/config` (koanf, YAML + `APP__` env + flags) | `crates/config` (`config` crate + serde, TOML files, same precedence and secret rules) | `figment` was rejected: no release since 2024, silently drops malformed env names. |
| `slog` + `logctx` | `tracing` + `tracing-subscriber` + `json-subscriber` | Typed `log.level` directive and `log.format`; `RUST_LOG` is not read. |
| OpenTelemetry Go traces and metrics + Prometheus | Traces: `opentelemetry` 0.32 + `tracing-opentelemetry` + `axum-tracing-opentelemetry`. Metrics: the `metrics` facade + `metrics-exporter-prometheus` + `axum-prometheus` + `metrics-process` + `tokio-metrics` | The facade is the Rust idiom; OTLP metric push is deferred. Diagnostics stay on a separate listener (`:9090`). |
| RFC 9457 `problem` package | `infra_http::problem` | Closed transport catalog, stable codes, no submitted values echoed; a `failure` leaf splits out with the gRPC profile. |
| oapi-codegen strict server, hand-written `service.yaml`, runtime request validator | `utoipa` + `utoipa-axum`: handlers carry the contract, the generated `api/openapi/service.yaml` is committed and byte-compared by a test, Redocly lints it, oasdiff compares it with the pull-request base | No maintained Rust-native spec-first server generator exists for axum; the JVM `openapi-generator` output was rejected on quality ([HTTP Architecture](architecture/http.md#decisions-recorded-here)). The committed document stays the reviewed authority. Extractors are the request validator. |
| `pgx` + `sqlc` + Goose | `sqlx` 0.9 (`postgres`, `runtime-tokio`, `tls-rustls-aws-lc-rs`, `migrate`): pool, transactions, embedded migrations under an advisory lock with checksums, `#[sqlx::test]` per-test databases; template-owned `Dsn` admission and commit-outcome policy in `crates/infra-postgres`; `crates/migrate` library plus binary; compose + `#[sqlx::test]` for proof | `refinery`, `diesel-async`, `sea-orm`, `testcontainers`, `cargo-nextest` rejected or deferred with reasons in [Persistence](architecture/persistence.md#decisions-recorded-here). `query!` macros with offline `.sqlx` metadata and `sqlx-cli` arrive with the first repository (stage 10). |
| River jobs | `infra-jobs` (stage 10.4): a template-owned PostgreSQL queue on `sqlx` 0.9, enqueued in the caller's transaction and run by the `jobs-worker` binary | No maintained crate with a stable release fills the four-part gap; [Async Architecture](architecture/async.md#decisions-recorded-here) records the candidates, the watch list, and the reopen conditions. |
| NATS JetStream (`nats.go`) | `async-nats` | |
| gRPC (`grpc-go`, buf) | `tonic` + `prost`, buf for lint and breaking checks | |
| `golangci-lint`, `depguard` | clippy workspace lints; crate graph for direction; `cargo-deny` bans for forbidden crates; `cargo-shear` for unused dependencies | |
| `gosec`, `govulncheck` | `cargo-deny` (advisories, licenses, bans, sources) and CodeQL for Rust; Trivy over the `cargo-auditable` binary in the image | No reachability analysis exists for Rust; `cargo-audit` would read the same database. |
| `goleak`, `-race` | Ownership and `Send`/`Sync` remove data races at compile time; task completion is proven by joining; `loom` only for hand-written lock-free code | Do not port a race detector step. |
| Distroless static image | Multi-stage build on `rust:<toolchain>-slim-trixie` with cargo-chef and `cargo auditable build`; `gcr.io/distroless/cc-debian13:nonroot` (glibc) runtime | Measured against musl (43.5 vs 9.1 MiB, equal build time); glibc keeps the tests' target triple and leaves the allocator decision to stage 11, which may switch to the static image. |
| `.agents/skills/go-*` | `.agents/skills/rust-*` (stage 7; `rust-delivery-platform` arrived with stage 4) | See the skills plan below. |

## Stages

### Stage 1: Bootstrap (done)

Delivered in this repository's first commits.

- Public GitHub template repository with topics, MIT license, code of conduct,
  security policy, issue forms, pull-request template, `CODEOWNERS`, and
  Dependabot for Cargo and GitHub Actions.
- Cargo workspace: `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`,
  workspace lints, workspace dependency versions, committed `Cargo.lock`.
- `crates/infra-http`: `Router` with `GET /health/live` and `GET /health/ready`
  (`text/plain`, `200 ok` / `503 not ready`), a shared `Readiness` flag, and a
  `Server` that binds, serves, and drains on request.
- `crates/service`: thin `main`, `bootstrap` that builds the runtime, installs
  logging, resolves `APP__HTTP__ADDR` (default `0.0.0.0:8080`, Go-style `:port`
  accepted), binds, flips readiness, waits for `SIGINT`/`SIGTERM`, flips
  readiness off, and drains HTTP inside a 25 s budget. Failures return a
  non-zero exit code through `ExitCode`, never `process::exit`.
- `Makefile` + `make/template.mk`: `build`, `run`, `test`, `test-package`,
  `fmt`, `fmt-check`, `lint`, `check`, `clean`.
- CI: one `quality` job (format, clippy, build, test) and the always-reported
  `required` job, with pinned action SHAs.
- `AGENTS.md` carrying the harness-neutral contract; `CLAUDE.md` pointer;
  this roadmap.

Exit criteria met: `make check` passes; the binary serves both probes, exits
`0` on `SIGTERM` after draining, and exits `1` with a readable message on a bad
address or an occupied port.

### Stage 2: Runtime core (done)

Research and decisions: absorbed in stage 5 into
[Configuration Source Policy](configuration-source-policy.md#decisions-recorded-here),
[HTTP Architecture](architecture/http.md#decisions-recorded-here),
[Runtime Lifecycle](architecture/runtime-lifecycle.md#decisions-recorded-here),
and [Component Boundaries](architecture/boundaries.md#decisions-recorded-here);
the research bundle (`specs/runtime-core/`) lives in Git history.

Delivered:

- `crates/config` (`service-config`): typed immutable snapshot over the
  `config` crate; precedence code defaults → `--config` → ordered
  `--config-overlay` → `APP__SECTION__KEY`; unknown keys and malformed
  variable names fail; secret-like values in files fail; `SecretString`
  fields; durations and byte sizes in human form; per-section validation with
  operator-readable messages. Policy: [Configuration Source Policy](configuration-source-policy.md).
- `crates/health`: background readiness refresher over `tokio::sync::watch`
  with failure threshold, staleness guard, and drain flag; `/health/ready` is
  an O(1) snapshot read.
- `crates/infra-http`: hardened chain from `tower-http` and `tower` (request
  id set/propagate with inbound validation, `nosniff`, OpenTelemetry server
  span, HTTP metrics, access log by route template, `503` shedding without
  queueing, `504` request timeout, sanitized `500` on panic, `413` body limit,
  `404`/`405` problems with `Allow`, fail-closed CORS by omission); RFC 9457
  `Problem` with the closed code catalog; a hyper accept loop with header
  timeout, header-size bound (`431`), connection cap, silent-client guard, and
  a bounded graceful drain.
- `crates/infra-telemetry`: subscriber (`json`/`text`), tracer provider with
  OTLP/HTTP export only when an endpoint resolves and the ambient-credential
  refusal, Prometheus recorder with process and Tokio runtime metrics, and the
  diagnostics router.
- `crates/service`: composition root with the ordered startup and the staged
  teardown under one grace deadline (readiness off → propagation delay →
  drain → diagnostics → background join → dependency close → telemetry
  flush), exit codes 0 / 3 (degraded shutdown) / 1, `vergen-gitcl` commit
  stamp, and process-level tests of the built binary.
- `env/config/local.toml` and `make run`.

Deviations from the Go template, with reasons, are tabulated in the
synthesis: TOML instead of YAML; no connection read/write/idle deadlines
(hyper has one header/idle knob); no `runtime.memory_limit_ratio` or `pprof`;
`metrics` facade instead of OpenTelemetry SDK metrics; tracer provider always
installed; `log.format` added; distinct exit code for a degraded shutdown.

Exit criteria met: every runtime key has a default, validation, a test, and a
policy line; `make check` passes (70 tests); the binary refuses unknown keys,
malformed names, and secrets in files; the shutdown sequence is observable in
logs and proven by the process test.

### Stage 3: OpenAPI-first contract (done)

Research and decisions: absorbed in stage 5 into
[HTTP Architecture](architecture/http.md#decisions-recorded-here) (candidate
survey outcome, deviations, deferred items, gotchas); the research bundle
(`specs/api-contract/`) lives in Git history.

Goal: `api/openapi/service.yaml` is the reviewed source of truth for the HTTP
contract, and the handlers cannot disagree with it.

Delivered:

- Code-first generation with `utoipa` 5.5 and `utoipa-axum` 0.2: the probe
  handlers in `crates/infra-http` carry `#[utoipa::path]` with
  `operationId`, `summary`, explicit public `security: []` where a root bearer
  default exists, and every
  response; `Problem` and `InvalidParam` derive their schemas from the
  serializer (`deny_unknown_fields` → `additionalProperties: false`); the
  shared problem responses are `ToResponse` components; the readiness
  handler returns an `IntoResponses` enum with one variant per status.
- `crates/service` gained a library (`api`) that merges every
  `OpenApiRouter` into one contract, finalizes the served router from its
  document, renders that document through an `openapi` binary, and has contract tests:
  committed file equals the generator output byte for byte, root and operation
  security produce an unambiguous effective policy, explicit public means
  `security: []`, and the problem schemas are closed. The
  `infra-http` router tests compare served media types with the declared
  ones for `200`, `503`, and `413`.
- `api/openapi/service.yaml` (OpenAPI 3.1, health-only), `.redocly.yaml`
  ported unchanged, `make openapi-generate`, `openapi-check`,
  `openapi-lint` (Redocly CLI 2.53.3 through `npx`, part of `make check`),
  `openapi-breaking BASE_OPENAPI=…` (oasdiff 1.32.1 through `go run`), CI
  lint step and pull-request compatibility step that skips when the base
  has no document.
- `docs/architecture/http.md` and the `rust-api-contract` skill.

Deviations from the Go template, with reasons, are tabulated in the
synthesis: code-first with the committed document as the reviewed authority;
OpenAPI 3.1 instead of 3.0.3; no runtime request validator (extractors and
the derived schema are one source); `Problem.code` stays a string in the
contract so the catalog can grow without a breaking change; optional members
declared non-nullable; `info` from the service crate's Cargo metadata; no
bearer scheme until the authentication profile; Redocly alone (its `struct`
rule validates structure); the drift and contract checks are ordinary Cargo
tests.

Exit criteria met: `make openapi-generate` on the committed document
produces no diff; a changed `summary` in the committed file fails the drift
test at the changed line; the probes are served by the annotated handlers
whose responses the document declares; oasdiff reports no breaking change
between the Go template's health-only 3.0.3 document and this one;
`make check` passes (80 tests).

### Stage 4: Validation routing and delivery (done)

Research and decisions: absorbed in stage 5 into
[CI/CD Production Readiness](ci-cd-production-ready.md#decisions-recorded-here)
(tool survey outcome, gate decisions, routing, image measurements,
deviations, deferred items, gotchas); the research bundle
(`specs/validation-delivery/`) lives in Git history.

Delivered, in five pull requests
([#8](https://github.com/Dankosik/rust-service-template-rest/pull/8),
[#9](https://github.com/Dankosik/rust-service-template-rest/pull/9),
[#10](https://github.com/Dankosik/rust-service-template-rest/pull/10),
[#12](https://github.com/Dankosik/rust-service-template-rest/pull/12),
[#13](https://github.com/Dankosik/rust-service-template-rest/pull/13)) plus
this documentation change:

- `tools/versions.env`, the one pin manifest; `make tools-check` proves the
  Cargo tools resolve, the Dockerfile `ARG` defaults agree, every `FROM`
  carries a digest, and the builder tag equals the toolchain channel.
- Dependency and secret gates: `deny.toml` + `make deny` (Linux gnu targets,
  the `paste` advisory ignore with its reopen condition, permissive license
  allow-list, path wildcards allowed, duplicate versions as warnings,
  crates.io only); `make unused-deps` (cargo-shear, which removed the unused
  `http` and `hyper` from `infra-http`); `.gitleaks.toml` + `make secret-scan`
  and `secret-scan-history`; `make actionlint`, `make zizmor`,
  `make shellcheck`.
- The validation system: `scripts/ci/changed-surfaces.sh` (16 surfaces,
  fail-closed, `--union BASE`, `--all`), `affected-crates.sh` (`cargo tree
  -i` reverse closure, test-only `tests/`, workspace fallback),
  `git-changed-paths.sh`, `validation-lock.sh`, `measure.sh`, and `verify.sh`
  (plan, attempt record, receipt keyed by candidate × plan × environment,
  invalidation, `ALLOW_HEAVY` and Docker preflight), each with a self-test;
  `make plan`, `make verify`, `lint-changed`, `test-changed`;
  `ALLOW_FULL=1 make check` under the validation lock.
- CI by surface: `changes` → `quality`, `security`, `secrets`, `delivery`,
  `image` → `required`; `codeql.yml` for Rust and Actions with
  `codeql-required`; Dependency Review on pull requests; weekly schedule over
  every surface; caches restored always and saved only on pushes to `main`;
  every action pinned by SHA.
- `build/docker/Dockerfile` and `.dockerignore`; `runtime-image-build.sh`
  and `runtime-image-check.sh`; `make dockerfile-check`,
  `runtime-image-build`, `runtime-image-check`, `container-security`,
  `container-sbom`; Dependabot for the `FROM` digests.
- `cd.yml` (opt-in through `ENABLE_GHCR_PUBLISH`) and
  `.github/actions/publish-image` with `publish-image-metadata.sh`; nothing
  published.
- `docs/validation-routing.md`, `docs/validation/{rust,security,delivery,
  containers,generated,instructions}.md`, `docs/ci-cd-production-ready.md`,
  `docs/railway-deployment-profile.md`, the `rust-delivery-platform` skill,
  and the `AGENTS.md` validation budget naming `ALLOW_FULL=1 make check` and
  the plan/verify route.

Deviations from the Go template, with reasons, are tabulated in the
synthesis: a versions file instead of a tools module; `cargo-deny` and CodeQL
instead of `govulncheck` and `gosec`; `cargo-shear` and `--locked` instead of
`go mod tidy`; glibc on `distroless/cc-debian13` with the toolchain file kept
out of the image context; `cargo auditable build` so the image scan sees Rust
packages; the Cargo version stays `app.version` and the lifecycle check
asserts `app.commit`; cargo-chef layers instead of cache mounts; no
`railway.toml` (Config as Code is deprecated) but a profile document with an
IaC snippet; a `target/` allowlist instead of a Gitleaks baseline; `cargo
test` instead of a runner. Implementation notes recorded during the stage:
the unmatched `ISC` licence allowance was dropped; actionlint's host
integrations are disabled; the repository dependency graph had to be enabled
for Dependency Review; base images are pinned in the Dockerfile only; the
SBOM comes from the pinned Trivy container; a release tag must equal the
crate version; two zizmor findings on `cd.yml` are ignored inline with their
reasons.

Exit criteria met: a docs-only pull request runs no Rust job
([#11](https://github.com/Dankosik/rust-service-template-rest/pull/11):
`quality`, `security`, `delivery`, and both CodeQL analyses skipped); a
Dockerfile change builds and lifecycle-checks the image in CI (`image` job on
#12: ready, `app.commit` asserted, clean stop in 15 s, Trivy clean on both
targets); `make verify` prints a plan and writes a receipt (locally: a
49 s route including the image gates); `ALLOW_FULL=1 make check` passes
(80 tests, 17 skills, 4 self-tests); every tool pin lives in one file. The
planted-finding check of the secret and dependency gates follows in a test
branch that is closed without merge.

### Stage 5: Repository documentation (done)

Research and decisions: absorbed into
[CI/CD Production Readiness](ci-cd-production-ready.md#decisions-recorded-here)
(the link checker) and this section; the research bundle
(`specs/repository-documentation/`) lives in Git history.

Delivered, in four pull requests
([#16](https://github.com/Dankosik/rust-service-template-rest/pull/16),
[#17](https://github.com/Dankosik/rust-service-template-rest/pull/17),
[#18](https://github.com/Dankosik/rust-service-template-rest/pull/18), and
the closing one):

- `make docs-check`: lychee in a digest-pinned container, offline, with
  `#fragment` resolution, over every tracked Markdown file; the
  `documentation` surface runs it in `make verify`, a CI `docs` job, and
  `ALLOW_FULL=1 make check`.
- `docs/repo-architecture.md` front door with global invariants, the
  source-of-truth table, and one leaf per pressure;
  `docs/architecture/boundaries.md`, `runtime-lifecycle.md`,
  `integration.md`; `http.md` and `configuration-source-policy.md` extended
  with the stage 2 and 3 decisions.
- `docs/project-structure-and-module-organization.md` (placement table and
  algorithm, filename rules, test placement, generated and proof
  boundaries), `docs/build-test-and-development-commands.md` (from the real
  `make help`), `docs/production-contract.md` (unresolved, service-owned).
- `docs/first-production-feature.md`, written from a walkthrough executed on
  a scratch worktree (a `greeting` crate, `GET /greetings/{name}`, merged in
  `service::api`, `ALLOW_FULL=1 make check` green with 84 tests). The
  walkthrough found and fixed the contract test that asserted the exact
  operation set.
- `CONTRIBUTING.md` completed against the catalog; `AGENTS.md` routes
  architecture pressure through the front door.
- The stage 2, 3, 4, and 5 research bundles absorbed into their owning
  documents and deleted; `specs/rust-skills/` stays open with stage 7.

Deviations from the Go template: a link checker where Go has none;
`test/README.md` deferred to the first `test/` crate (no directory before its
first artifact); `architecture/async.md` and `persistence.md` deferred to
their profiles; placement rules name the crate graph and `#[cfg(test)]` /
`tests/` instead of Go packages and depguard file families; the first-feature
guide edits handlers and regenerates the document (code-first contract).

Exit criteria met: `make docs-check` passes (169 links, 0 errors) and CI's
`docs` job runs it on every documentation change; every `AGENTS.md`
conditional owner resolves; the first-feature guide was followed on the
scaffold end to end.

### Stage 6: Agent harness and spec-first workflow (done)

Research: the Go template's harness documents and adapters are the research
for this stage (they record how each harness discovers instructions, skills,
and agents as of September 2026); no crate survey applies, and the port is
"path changes only" as the working rules prescribe. The decisions specific to
this repository are recorded in [Skill Authoring](skill-authoring.md#workflow-skills)
(two skill classes) and below.

Delivered in one pull request:

- `docs/spec-first-workflow.md` router; `phases/` (Intake, Research,
  Specification, System / Integration Design, Rust Code / Ownership Design,
  Planning, Implementation, and the reviews), `interfaces/` (result
  contracts V1), `shared/` (artifacts, evidence contract, external effects,
  review, resume, transition, cleanup, repository boundaries, read-only
  delegation), `rubrics/` (falsifier, material flow and rule, release
  closure, Rust ownership review).
- `docs/agent-harness.md` and adapters for Codex, Claude Code, Qwen Code,
  Grok Build, Cursor, and OpenCode; `docs/prompt-composition.md`,
  `docs/prompt-maintenance.md`, `docs/subagent-brief-template.md`.
- `.agents/roles` (five canonical roles), `.agents/role-classes`,
  `.agents/contracts` (specialist contract, arbitration, and a neighbor map
  written for the Rust skill catalog), `.agents/codex-project.toml`;
  generated carriers under `.claude`, `.codex`, `.cursor`, `.qwen`, `.grok`,
  `.opencode` from `scripts/agent-roles-sync.sh`, `scripts/codex-agents-sync.sh`,
  and `scripts/harness-skills-sync.sh`; the hand-maintained Lead and
  orchestrator carriers; `CLAUDE.md`, `QWEN.md`, `Grok.md`,
  `.cursor/rules/agent-harness.mdc`, `opencode.json`, and the harness
  settings files.
- The nine harness-neutral workflow skills (`orchestrator`,
  `acceptance-unit-lead`, `spec-first-brainstorming`,
  `spec-document-designer`, `idea-refine`, `planning-and-task-breakdown`,
  `grilling`, `agent-prompt-composer`, `thermo-nuclear-code-quality-review`)
  with their references and Codex carriers, and `merge-conflict-resolution`
  rewritten into the decision-skill shape; `scripts/check-skills.py` validates
  both classes.
- `make check-instructions` (skill shape plus the four carrier checks) in
  `ALLOW_FULL=1 make check`, in `make verify`, and in CI's `quality` job on
  the `agent_instructions` surface, whose classifier rows now cover every
  carrier, harness document, and sync script.
- `AGENTS.md` routes non-direct work through the router and names the
  harness, prompt, and external-effect owners; it has its final shape for
  the template.

Deviations from the Go template: the `Go Code / Ownership Design` phase and
`Go Ownership Review` rubric are the Rust ones and judge the crate graph,
`pub(crate)` visibility, and `#[cfg(test)]`/`tests/` placement; the phase
method lists name the `rust-*` skills that exist and say where a pressure
without a skill (domain invariants, data truth, durable delivery) is decided;
decision skills carry no `metadata` block, so the skill sync treats its
absence as `invocation: model`, `kind: method`; the specialist neighbor map
is written for the eighteen decision skills rather than translated; there is
no template-sync or purity check yet (stage 9), so `check-instructions` is
the aggregate of the carrier checks.

Exit criteria: every carrier check passes (`make check-instructions`, 27
skills, 5 roles for 6 harnesses); `AGENTS.md` has its final shape. Binding
`/orchestrator` and dispatching a Lead is structurally in place (the skill,
the Cursor and Codex carriers, and the adapters exist; Cursor discovers
`.agents/skills` in this repository) but has not been exercised on a real
ledger at that stage. Stage 9's [completion record](../specs/template-initializer/completion.md#first-dispatched-ledger)
now records the first real dispatch, joined Implemented return, immutable
candidate custody and serial local integration. These exercise the carried
harness obligation separately from behavior and CI validation.

### Stage 7: Rust backend skills (core set done)

Goal: `.agents/skills/rust-*` gives agents the same decision coverage as the
Go template's `go-*` set, built from
[rust-cli-skills](https://github.com/Dankosik/rust-cli-skills) where its
decisions carry over.

Delivered after stage 2 (decisions in `specs/rust-skills/research/synthesis.md`):
fifteen skills under `.agents/skills` in the rust-cli-skills shape (`rust-coder`,
`rust-idiomatic`, `rust-tokio`, `rust-axum`, `rust-errors`, `rust-config`,
`rust-observability`, `rust-reliability`, `rust-security`, `rust-testing`,
`rust-performance`, `rust-debugging`, `rust-structural-quality`,
`rust-dependencies`, `rust-verification`), each grounded in the repository
owner it decides against; [Skill Authoring](skill-authoring.md);
`make check-skills` (frontmatter, name/directory agreement, trigger, prose-only
body, word budget, LICENSE copy) wired into `make check` and CI; `AGENTS.md`
routing to the catalog. `rust-api-contract` arrived with stage 3 and
`rust-delivery-platform` with stage 4; the harness-neutral workflow skills,
`merge-conflict-resolution`, and the Claude/Qwen views with stage 6.
`rust-sqlx` arrived with stage 8 (the `persistence` neighbor cluster and
its edges to `rust-reliability`, `rust-errors`, `rust-security`,
`rust-testing`, `rust-config`). Stage 10.4 ported the first universal
discipline, `durable-background-jobs`, byte for byte from the Go template;
`rust-reliability` reaches it, with its static fixture in
`evals/rust-reliability/durable-background-jobs.md`. Remaining for this
stage: capability skills with their stages (`rust-tonic`, the profile
skills), the other universal disciplines when a capability reaches them, and
behavioural evaluation fixtures under `evals/` now that the reviewer roles
exist.

Original mapping from the CLI pack:

From rust-cli-skills, keep the decision and rewrite the examples and evidence
rules for a long-running Tokio service:

| rust-cli-skills | Template skill | Change |
| --- | --- | --- |
| `rust-implement` | `rust-coder` | Add earliest-owner, far-side observer, generated/manual authority, and ledger completion semantics from `go-coder`. |
| `rust-idiomatic` | `rust-idiomatic` | Add `Send`/`Sync` bounds at async boundaries, `Arc` sharing rules, error identity across crates. |
| `rust-design` | `rust-structural-quality`, `rust-language-simplifier` | Split as in the Go set: cohesion and ownership vs. behavior-preserving simplification. |
| `rust-errors` | `rust-errors` | Replace exit-status guidance with problem mapping, `?` across crate boundaries, and `thiserror`/`anyhow` ownership (typed in libraries, opaque only at the composition root). |
| `rust-concurrency` | `rust-tokio` | Task lifetime, `JoinSet`, cancellation safety across `select!`, `spawn_blocking` limits, graceful shutdown ordering, bounded channels and backpressure, lock guards across `await`. |
| `rust-memory`, `rust-performance` | `rust-performance` | Allocation and retention in request handlers, streaming bodies, connection pools; evidence via criterion and k6. |
| `rust-debugging` | `rust-systematic-debugging` | Add `tokio-console`, tracing spans, and flaky async reproduction controls. |
| `rust-testing` | `rust-test-strategy`, `rust-test-implementation` | Proof obligations and oracles; `tower::ServiceExt::oneshot`, process tests, PostgreSQL integration proof, deterministic time with `tokio::time::pause`. |
| `rust-build` | `rust-modern-version`, `rust-delivery-platform` | Edition and MSRV changes; release profile, image, and CI gate ownership. |
| `rust-cli-interface`, `rust-cli-testing`, `rust-filesystem`, `rust-processes`, `rust-io`, `rust-distribution` | dropped | CLI-specific; the relevant pieces (bounded I/O, child processes for migrations) fold into the skills above. |

New skills mirroring the Go set: `rust-axum` (route tree, layers, extractors,
fallbacks, route labels), `rust-api-contract`, `rust-observability`,
`rust-reliability`, `rust-security`, `rust-sqlx` (after stage 8),
`rust-data-architecture`, `rust-domain-invariant`, `rust-distributed`,
`rust-system-architecture`, `rust-implementation-ownership`,
`rust-verification-before-completion`, `rust-tonic` (with the gRPC profile).
Harness-neutral skills (`orchestrator`, `acceptance-unit-lead`,
`spec-first-brainstorming`, `spec-document-designer`, `idea-refine`,
`planning-and-task-breakdown`, `grilling`, `agent-prompt-composer`,
`merge-conflict-resolution`, `thermo-nuclear-code-quality-review`) ported
with path changes only in stage 6.

`docs/universal-disciplines/` is language-neutral and ports verbatim except
its "reached from" column.

Each skill follows `docs/skill-authoring.md`: description as a routing
discriminator, one decision, checkable completion, references only for
branch-only pressure. Add contrasting evaluation fixtures under `evals/` for
any skill whose decision differs from its Go or CLI source.

Exit criteria: skill catalog validates; the neighbor map has no collisions;
each new or changed skill has at least one positive and one negative fixture.

### Stage 8: PostgreSQL profile (done)

Goal: `postgres.enabled = true` adds a pool, migrations, transactions, and
proof; the profile is inert otherwise.

Research: `sqlx` 0.9 against `tokio-postgres` + `deadpool-postgres` +
`refinery`, `diesel-async`, `sea-orm`, and `bb8-postgres`; compose +
`#[sqlx::test]` against `testcontainers`; `cargo-nextest` reopened and not
adopted. The driver's behavior was verified in a scratch project against
`postgres:18.4` (environment overlay, `sslmode` parsing, session defaults
through the startup packet, `lock_timeout` on `pg_advisory_lock`, checksum
mismatch, per-test databases). The synthesis was absorbed into
[Persistence Architecture](architecture/persistence.md#decisions-recorded-here)
and deleted.

Delivered in one pull request:

- `crates/infra-postgres`: `Dsn` admission mirroring the Go rules (URL form,
  explicit `sslmode` in `disable`/`require`/`verify-ca`/`verify-full`, no
  libpq environment, passfile, socket, fallback host, TLS file, or unknown
  parameter; diagnostics never carry the value), the pool with `acquire` 3 s
  and session `statement_timeout`/`idle_in_transaction_session_timeout` 8 s,
  `PostgresProbe`, `db_client_connection_count` gauges, `in_tx`/`in_tx_with`
  over an `AsyncFnOnce` with `CommitFailed` versus `CommitUnknown` and
  `retryable`.
- `crates/migrate`: `MIGRATOR` embedded from `migrations/`, `run` over one
  connection (`lock_timeout` 15 s, statement 2 min, deadline 5 min) with
  stages `source`/`connect`/`lock`/`state`/`execute`/`deadline`, source rules
  as a test, the `migrate` binary with one terminal `migration_run` record;
  `migrations/README.md` states the rules (the set is empty until the first
  durable feature).
- `service-config` `postgres` section; bootstrap opens the pool before
  admission, registers the probe and the gauge task, closes the pool in the
  dependency-close stage, and cleans up a partial startup.
- `env/docker-compose.yml` (`postgres:18`, digest-pinned, Dependabot
  `docker-compose`); `test/` (`integration-tests`) with fifteen
  database-backed tests behind the `integration` feature and fixture
  migration sets; `scripts/ci/test-integration-db.sh`,
  `migration-validate.sh`, `migration-history-check.sh` (with self-test),
  `scripts/lib/compose-postgres.sh`; `runtime-image-check.sh` learns
  `RUNTIME_IMAGE_NETWORK` and `RUNTIME_IMAGE_POSTGRES_DSN`.
- Make: `compose-up`, `compose-down`, `test-integration-db`,
  `migration-check`, `migration-history-self-test`, `migration-validate`;
  `lint` covers the feature-gated tests; `check` gains `migration-check`.
- Surfaces `db_integration` and `migrations`; CI `integration` job, the
  migration steps in `quality`, the rehearsal in `image` in place of the
  plain lifecycle check when migrations changed; `make verify` plans them.
- The image builds and ships `/migrate`; `.dockerignore` admits
  `migrations/` and the `test/` manifest.
- `deny.toml` admits ISC and CDLA-Permissive-2.0 for the TLS stack.
- Docs: [Persistence Architecture](architecture/persistence.md),
  [PostgreSQL Validation](validation/postgres.md), `test/README.md`,
  `migrations/README.md`; boundaries, lifecycle, configuration policy,
  routing, structure, commands, CI/CD, README, CONTRIBUTING, AGENTS updated;
  the `rust-sqlx` skill.

Deviations from the Go template are listed in the persistence document:
embedded forward-only migrations instead of a runtime directory with Goose
sections, the advisory lock bounded by `lock_timeout` instead of a locker
with its own timeouts, a dropped connection instead of a cancel request at
the deadline, `max_connections` instead of `max_open_conns`, the callback
error returned and the rollback failure logged instead of `errors.Join`,
compose + `#[sqlx::test]` instead of `testcontainers`.

Exit criteria met: the default configuration starts without PostgreSQL and
`make test` needs no Docker (the database tests compile with their feature
off); `ALLOW_HEAVY=1 make test-integration-db` passes fifteen tests on
`postgres:18`; `ALLOW_HEAVY=1 make migration-validate` migrates a fresh
database from the image, replays as `no_change`, and passes the lifecycle
check with the pool open; CI runs the proof on the `db_integration` surface
and the rehearsal on `migrations`. Orchestration: the stage was small enough
for one actor with the research synthesis as its artifact; the harness'
first dispatched ledger is exercised by stage 9, with dispatch, joined return
and integration custody recorded in its
[completion record](../specs/template-initializer/completion.md#first-dispatched-ledger).

### Stage 9: Template initializer, profiles, and sync

State: delivered by this change; completion takes effect on merge after the
required CI gates pass.

Goal: `make template-init` turns the template into one service, and
`template-sync.sh` propagates portable instructions to derived repositories.

- `scripts/init-module.sh` equivalent: rewrites the service crate and binary
  name, description, `CODEOWNERS`, and `README` identity; selects profiles;
  removes unselected profile packs by marker; records `template.lock`;
  rejects unsupported combinations before mutation.
- Whole-line profile markers `template:begin postgres:<id>` and
  `template:end postgres:<id>` use the host comment wrapper (`#`, `//`, or
  `<!-- ... -->`). Registered identifiers are unique lowercase hyphenated
  names with exact source paths in `scripts/lib/template_profiles.json`.
  Mixed Cargo, Rust, Dockerfile, configuration, CI and local documentation
  owners carry them; generated OpenAPI and portable owners never do.
- `template-owned.paths`, `scripts/template-sync.sh` with `--check`,
  `--apply`, `--instructions-only`, all refusal rules, and the purity check.
- `make template-init-check` matrix in CI on the `module_initializer` surface.
- `docs/template-sync.md`.

Exit criteria: every supported profile combination initializes, builds, and
passes `make check` in the CI matrix; a derived repository syncs cleanly.

The [accepted contract](../specs/template-initializer/spec.md) fixes a 2 × 8
matrix: database `none`/`postgres` and harness
`core`/`codex`/`claude`/`qwen`/`cursor`/`grok`/`opencode`/`all`.
[Technical Design](../specs/template-initializer/design/system.md) closes the
committed Git snapshot, one-shot JSON provenance, guarded locked Cargo
projection, exact ownership admission and consumer-preserving settings/skill
mechanisms. The [local completion record](../specs/template-initializer/completion.md)
retains the assembled implementation, proof and independent review evidence.

Local implementation and validation are complete: the public sixteen-output
matrix and source safety/purity/sync suites passed, retained real PostgreSQL
and image lifecycle proof passed, and independent integrated review passed.
Exact candidates, commands and scoped reuse are in the completion record.
The first native dispatched-ledger exercise completed dispatch, the five
descendants' joined
Implemented return, immutable candidate handoff and serial local integration;
the completion record preserves native actor identity and custody. Source-only
static instruction fixtures cover reached stage-7 portability/preservation
decisions; they make no measured model-behavior
claim and add no universal discipline or capability skill.

The delivery pull request closes this stage when its exact head passes the
required CI gates, including the sixteen-output initializer matrix and sync
suites, and lands on `main`. The completion record preserves the earlier local
acceptance boundary separately from delivery. Neither acceptance establishes
publication, deployment or stage-10 work.

### Stage 10: Optional capability profiles

Each profile is its own sub-stage with a guide under `docs/`, an inert default,
markers, tests, and initializer support. Order by expected demand:

1. Authentication: `AUTHN=oidc-jwt` (JWKS discovery and refresh, token
   profiles) and `AUTHN=oidc-introspection`; bearer grammar and sanitized
   failure taxonomy in `crates/infra-bearerauthn`. **Merged via PR #41 at
   `098b4ab18dd5b2d158a94e126798d8cc429ad735`**;
   [adoption and durable decisions](authentication.md).
2. Bounded outbound HTTP: fixed trusted-origin `reqwest` client with normal
   TLS/system resolution, finite header-count and encoded-body limits,
   correlation stripping, no proxy, and bounded telemetry. Its current
   [adoption guide](outbound-http.md) and [decision record](outbound-http-decisions.md)
   define the profile.
3. HTTP idempotency on PostgreSQL: composed operations, replay evidence and
   business effect in one transaction. **Merged via PR #49 at
   `4819113b21c110e69f3f1d4d26f3bf9337c83b72`**;
   [adoption guide](http-idempotency.md).
4. Durable background jobs on PostgreSQL and the `jobs-worker` binary.
   **Merged via PR #51 at
   `48e565af7c2875832996816b975a2e2b01457d4f`**;
   [adoption guide](background-jobs.md).
<!-- template:begin webhooks-common:roadmap-stage-10-5-webhooks -->
5. Outbound and inbound webhooks: `WEBHOOKS=durable` and
   `INBOUND_WEBHOOKS=standard-webhooks` independently retain static,
   environment-secret-backed Standard Webhooks v1 capability. Both require the
   PostgreSQL jobs pack; outbound also requires bounded outbound HTTP. Reuse
   `infra-jobs` scheduling, attempts, deadline, and fenced completion; do not
   add a second queue, worker, lifecycle crate, acceptance ledger, or endpoint
   management API. The initializer's target is 46 runtime graphs plus 368 cheap projections:
   the 26 baseline graphs, three full capability shapes, and 17 focused
   cross-profile combinations over the retained eight CI partitions. This
   describes required proof design, not a completed CI or delivery receipt.
<!-- template:end webhooks-common:roadmap-stage-10-5-webhooks -->
<!-- template:begin webhooks:roadmap-stage-10-5-outbound-guide -->
   The [outbound guide](outbound-webhooks.md) owns outbound adoption,
   retention, and delivery limits. Outbound acceptance is caller-transactional
   and preserves a stable retry identity.
<!-- template:end webhooks:roadmap-stage-10-5-outbound-guide -->
<!-- template:begin inbound-webhooks:roadmap-stage-10-5-inbound-guide -->
   The [inbound guide](inbound-webhooks.md) owns receipt admission,
   processing, and retention limits. Raw-byte verification atomically creates a
   PostgreSQL receipt and processing job with duplicate/conflict arbitration.
<!-- template:end inbound-webhooks:roadmap-stage-10-5-inbound-guide -->
6. NATS JetStream messaging with typed domain events and a `worker` binary;
   transactional outbox with an `outbox-relay` binary. Reuse `infra-jobs` for
   durable local scheduling and completion where that boundary applies; the
   messaging/outbox design owns its distinct delivery semantics.
7. gRPC with `tonic`: server policy, interceptors, health, bounded drain,
   shared client connections, buf lint and breaking checks.
<!-- template:begin outbound-auth:roadmap-stage-10-8-outbound-auth -->
8. OAuth 2.0 client-credentials outbound authentication:
   `OUTBOUND_AUTH=oauth2-client-credentials` retains private per-integration
   token reuse over bounded outbound HTTP; it is otherwise absent. The
   [adoption guide](outbound-machine-authentication.md) and [decision
   record](outbound-machine-authentication-decisions.md) define the profile.
<!-- template:end outbound-auth:roadmap-stage-10-8-outbound-auth -->
9. S3-compatible object storage with one fixed endpoint.
10. `examples/reference-service`: one isolated vertical slice.

Stage 10.1 local evidence includes workspace build/tests, the generated
contract, real local TLS and mounted HTTP authentication cases, dependency
policy and existing initializer safety/sync proof. All six DATABASE × AUTHN
runtime graphs passed initialized build/full checks; those scoped results were
reused after exact input-equivalence checks during a validation-only refactor.
The current strict 48-projection run completed in 31 seconds on this workspace
(receipt `attempt.TIn1zM`), with a fresh validation review PASS. This is measured
local execution, not a portable benchmark. The earlier run stopped after 18
completed cells and is not a 48-cell aggregate PASS.

The stage-10.1 [initializer proof owner](template-sync.md#validation-boundary) separated
48 canonical projections from six public-CLI/build/test representatives;
it did not repeat the full aggregate per harness. Immutable local custody is
under `.git/codex/authn-delivery/`, including source `c24c3b3ccfaf0af06f3bd40427930221b7be286f`
and the final closeout bundle. No main-checkout commit, CI success, publication
or deployment is claimed by that local acceptance.

Stage 10.2 retains `infra-outbound-http` for an operator-selected trusted HTTPS
origin. It uses normal system resolution and normal TLS verification, including
private or loopback providers whose configuration, certificate, and deployment
network establish trust. It is not an SSRF boundary and must never be
constructed from a caller-controlled URL. The auth client keeps its independent
JWT/introspection transport policy. The selected pack is inert until a provider
constructs it; the default initializer removes it unless outbound HTTP retains
it. The [guide](outbound-http.md) owns use and limits, while the
[decision record](outbound-http-decisions.md) retains version evidence and
reopen conditions. The previous public-address/DNS admission evidence applied
to a superseded contract and is not evidence for this delta. No remote CI, PR,
publication, deployment, live-provider, or PostgreSQL-runtime result is claimed.

Stage 10.3 adds `HTTP_IDEMPOTENCY=none|postgres`, which requires
`DATABASE=postgres` and an authentication engine. The selected pack holds the
`infra-idempotency-store` record store, the `infra_http::idempotency` seam
with four catalog codes, the `http_idempotency.retention` setting, bootstrap
activation before readiness admission, and canonical forward migrations. It
stays inert until an operation composes through `Composer::route`; `none`
removes it. Identity comes from the complete bounded HTTP request and decoded
key, `execute(work)` uses the shared `infra-postgres` transaction capability,
and durable replay retains seven byte-preserving headers plus trusted caller
metadata. The current [guide](http-idempotency.md) and architecture documents
are authoritative. The following receipt is historical evidence for the
earlier PR #49 implementation; it does not prove the current implementation
or its tests.

Stage-10.3 local acceptance ran every local step of the `make plan` route:
formatting, the workspace lint including the integration feature, build, 403
workspace tests, the contract check, dependency and secret gates, workflow
and shell checks, migration checks, and documentation. It also ran the
real-PostgreSQL suite: the store-boundary claims P1-P8, the mounted-router
claim P9 with the real introspection verifier, and the existing PostgreSQL
proof, none skipped. All 128 canonical projections passed, and 16 runtime
graphs were each initialized, built, and tested once. Graphs 13-16 also ran
the idempotency database suite, with P9 in 15-16. A one-shot comparison found
the generated contract and projected `Cargo.lock` of all twelve `none` graphs
equal to the `d24d173` baseline (24 digests), and the independent final
review passed. Repairs during validation (lint, one test composition, one
initializer test fixture, one guide link) each reran only the evidence they
invalidated. The first full matrix attempt remains recorded as failed in its
source suites, and the documentation-only repair reran the projections alone,
not the aggregate. Local custody is under
`.git/claude/http-idempotency/delivery/`. The exact-head
[CI run](https://github.com/Dankosik/rust-service-template-rest/actions/runs/35986060000)
of PR #49 then passed `required`, including the runtime image build, the
migration rehearsal, the image vulnerability scan, the database suite, and
all four initializer parts, and
[CodeQL](https://github.com/Dankosik/rust-service-template-rest/actions/runs/35986059988)
passed `codeql-required`. Publication and deployment are not claimed.

Stage 10.4 adds `JOBS=none|postgres`, which requires `DATABASE=postgres`.
The selected pack holds the `infra-jobs` crate (enqueue inside the caller's
transaction, the job-kind contracts, and the engine over one
`background_jobs` table: fenced claims, persisted backoff, lost-worker
recovery, and retention), the `jobs-worker` library and binary shipped as
the image's `/jobs-worker` entrypoint, the `jobs.max_workers` setting, the
canonical forward-only migrations, the [guide](background-jobs.md), and
[Async Architecture](architecture/async.md). It stays inert: the service
makes no jobs query, and nothing starts a worker until an operator deploys
`/jobs-worker`; `none` removes it.

Stage-10.4 local acceptance ran every local step of the `make plan` route:
tool pins, the validation-routing self-tests, dependency gates, formatting,
the workspace lint including the integration feature, build, 471 workspace
tests, workflow and shell checks, instruction checks, migration checks, the
Dockerfile check, and documentation. A one-shot comparison found the
generated contract and projected `Cargo.lock` of all sixteen `JOBS=none`
graphs equal to the `966aad8` baseline (32 of 32 digests), and the
independent final review passed. Repairs during validation (the workspace
lint over `infra-jobs`, its database tests, and one worker unit test; the
guide's empty-kind refusal text; one reopen condition in the architecture
leaf, corrected once) each reran only the evidence they invalidated. Local
custody is under `.git/claude/background-jobs/delivery/`. The exact-head
[CI run](https://github.com/Dankosik/rust-service-template-rest/actions/runs/36082103573)
of PR #51 then passed `required`, including the real-PostgreSQL suites
(enqueue, execution with two racing engines, the worker process suite, the
joint HTTP idempotency module, and the existing database proof), the 208
canonical projections and all eight initializer parts over the 26 runtime
graphs, the runtime image build with its `/jobs-worker` step, the migration
rehearsal, and the image vulnerability scan, and
[CodeQL](https://github.com/Dankosik/rust-service-template-rest/actions/runs/36082103697)
passed `codeql-required`. That run took 464 s from the first job's start to
`required`, against the 373 s baseline. The image job set it at 452 s: its
build step grew from 293 s to 406 s with the third release binary. The
slowest initializer part took 278 s, inside the image job, so the
initializer stayed off the critical path. Publication and deployment are not
claimed.

The subsequent jobs simplification retains that profile boundary while
replacing lease upkeep and outcome attribution with a fixed lease and
supervisor-owned outcome. Its canonical migration stores JSONB payloads and
C-collated text unique keys. Future stage 10.5 (webhooks) and 10.6
(messaging/outbox) reuse the jobs scheduling, attempt, and fenced-completion
mechanisms. Process lifecycle ownership stays separate until a shared signal,
deadline, or tracked-task teardown change justifies extraction; another binary
alone does not.

### Stage 11: Benchmarking and performance evidence

- `criterion` microbenchmarks with `make benchmark-capture` and
  `benchmark-compare`; k6 HTTP harness with `make benchmark-http`; evidence
  owner and workload contract in `docs/benchmarking.md`.
- Release profile decision (LTO, codegen units, allocator) from measurements,
  and optional PGO via `cargo-pgo` with the same verification discipline the
  Go template applies to its profile manifest.

### Stage 12: First release and derived-repository verification

- Create a derived repository from the template, run the initializer for the
  minimal profile and for one PostgreSQL profile, build, test, and deploy the
  image once.
- Tag `v0.1.0`, publish release notes, update repository topics and README
  badges, and list the template where the Go template is listed.

## Working rules for every stage

- Research before implementation. Before a stage starts, survey the crates
  and frameworks that already solve each problem in the stage: verify the
  latest version, release date, maintenance status, and exact behavior against
  the stage's requirements from primary sources (docs.rs, crates.io, the
  repository), and record the comparison and the decision in
  `specs/<stage-topic>/research/synthesis.md`. Prefer a maintained crate over
  template-owned code whenever it meets the requirement idiomatically; write
  custom code only for a named gap the synthesis documents.
- Rust idiom outranks Go parity. The Go template supplies the *problem* and
  the *reasons* behind each decision, not the shape of the solution. When the
  Rust ecosystem solves the same problem differently (a different layering,
  a type-system guarantee instead of a runtime check, a different file
  format, a facade crate instead of an SDK), do it the Rust way and record
  the deviation and its rationale in the synthesis. Copy a Go mechanism
  one-to-one only when Rust has no established alternative.
- One stage, or one profile inside stage 10, per pull request series. Update
  the status table in this file in the same pull request that completes a
  stage.
- Read the Go template's file for the concept being ported before writing the
  Rust version; port the decision and its reasons, then remove anything Rust
  already guarantees.
- Do not create a directory before its first real artifact. Do not leave
  placeholder modules, empty profiles, or dormant configuration keys.
- Every link in `AGENTS.md` and `docs/` must resolve to a file in this
  repository at the commit that introduces it.
- Record a non-obvious decision (framework choice, generator strategy,
  database crate) in `specs/<topic>/` while it is open, then move the accepted
  outcome into the owning document and delete the bundle.
