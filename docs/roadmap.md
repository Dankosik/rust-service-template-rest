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
| 5 | Repository documentation: architecture, placement, commands, production contract | planned |
| 6 | Agent harness and spec-first workflow | planned |
| 7 | Rust backend skills and universal disciplines | in progress: core set done, capability skills arrive with their stages |
| 8 | PostgreSQL profile | planned |
| 9 | Template initializer, profiles, and template sync | planned |
| 10 | Optional capability profiles | planned |
| 11 | Benchmarking and performance evidence | planned |
| 12 | First release and derived-repository verification | planned |

Stages 2 and 3 are independent of each other; 4 and 5 depend on both. Stage 6
depends on 5. Stage 7's core set was pulled forward after stage 2 so the
remaining stages are implemented through skills; its capability skills and
the Claude/Qwen discovery views follow their stages. Stage 9 depends on 4, 5, and 8. Stage 10 items are
independent of each other and each depends on 9 for its profile marker.

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
| `tools/go.mod` pinned developer tools | `tools/versions.env`: one `NAME=value` pin per tool, read by `make`, shell, and CI; Cargo tools (`cargo-deny`, `cargo-shear`, `zizmor`) built once per version into the Git common directory locally, prebuilt in CI through `taiki-e/install-action`; Go tools through `go run`; ShellCheck and Trivy as digest-pinned containers | Cargo has no project-local tool table; `make tools-check` proves the pins resolve. `cargo-nextest` reopens at stage 8, `cargo-audit` and `cargo-machete` were rejected (`specs/validation-delivery/research/synthesis.md`). |
| `cmd/service/main.go` → `bootstrap.Run` | `crates/service/src/main.rs` → `bootstrap::run` | Runtime construction, signals, and drain live in `bootstrap`; `main` only maps the result to an exit code. |
| `internal/<feature>` | `crates/<feature>` | Not created until the first feature exists. |
| `internal/infra/http` (`chi`, `net/http` server) | `crates/infra-http` (axum `Router`, `tower-http` layers, hand-rolled hyper accept loop) | Middleware order is the route tree's observable semantics in both. `axum::serve` exposes no connection limits and sets no timer, so the accept loop is template-owned. |
| `internal/config` (koanf, YAML + `APP__` env + flags) | `crates/config` (`config` crate + serde, TOML files, same precedence and secret rules) | `figment` was rejected: no release since 2024, silently drops malformed env names. |
| `slog` + `logctx` | `tracing` + `tracing-subscriber` + `json-subscriber` | Typed `log.level` directive and `log.format`; `RUST_LOG` is not read. |
| OpenTelemetry Go traces and metrics + Prometheus | Traces: `opentelemetry` 0.32 + `tracing-opentelemetry` + `axum-tracing-opentelemetry`. Metrics: the `metrics` facade + `metrics-exporter-prometheus` + `axum-prometheus` + `metrics-process` + `tokio-metrics` | The facade is the Rust idiom; OTLP metric push is deferred. Diagnostics stay on a separate listener (`:9090`). |
| RFC 9457 `problem` package | `infra_http::problem` | Closed transport catalog, stable codes, no submitted values echoed; a `failure` leaf splits out with the gRPC profile. |
| oapi-codegen strict server, hand-written `service.yaml`, runtime request validator | `utoipa` + `utoipa-axum`: handlers carry the contract, the generated `api/openapi/service.yaml` is committed and byte-compared by a test, Redocly lints it, oasdiff compares it with the pull-request base | No maintained Rust-native spec-first server generator exists for axum; the JVM `openapi-generator` output was rejected on quality ([HTTP Architecture](architecture/http.md#decisions-recorded-here)). The committed document stays the reviewed authority. Extractors are the request validator. |
| `pgx` + `sqlc` + Goose | Decision in stage 8 | `sqlx` with compile-time checked queries and offline metadata is the leading candidate; migrations via `sqlx migrate` or `refinery`. |
| River jobs | Decision in stage 10 | Candidates: `apalis`, `underway`, or a template-owned PostgreSQL queue. |
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
  `operationId`, `summary`, `x-security-decision`, `security: []`, and every
  response; `Problem` and `InvalidParam` derive their schemas from the
  serializer (`deny_unknown_fields` → `additionalProperties: false`); the
  shared problem responses are `ToResponse` components; the readiness
  handler returns an `IntoResponses` enum with one variant per status.
- `crates/service` gained a library (`api`) that merges every
  `OpenApiRouter` into one value whose halves are the served router and the
  document, an `openapi` binary that renders it, and contract tests: the
  committed file equals the generator output byte for byte, every operation
  declares its security decision, `public` means `security: []`,
  `protected` means the bearer scheme alone plus `400`/`401`/`403`/`431`/
  `503`/`504` problem responses, and the problem schemas are closed. The
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

Research and decisions: `specs/validation-delivery/research/synthesis.md`
(tool survey with verified behaviour, the surface table, the job layout, the
image measurements, deviations, and gotchas). The bundle stays open with the
stage 2 and 3 bundles until the stage 5 documents absorb them.

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

### Stage 5: Repository documentation

Goal: the same documentation graph as the Go template, rewritten for the
crate layout, with every link resolving.

- `docs/repo-architecture.md` front door with global invariants, source of
  truth table, and one-leaf selector.
- `docs/architecture/boundaries.md`, `runtime-lifecycle.md`,
  `integration.md`, `persistence.md` (after stage 8), `async.md` (after the
  first async profile); `http.md` exists since stage 3 and receives the
  stage 2 and 3 research bundles' durable decisions.
- `docs/project-structure-and-module-organization.md` with the deterministic
  placement algorithm for crates and modules, filename rules, and test
  placement (`#[cfg(test)]` beside the owner, `tests/` for black-box crate
  tests, `test/` workspace crate for process and container proof).
- `docs/build-test-and-development-commands.md`, `docs/production-contract.md`,
  `docs/first-production-feature.md`, `specs/README.md`, `test/README.md`.
- `CONTRIBUTING.md` completed against the real command catalog.

Exit criteria: a link checker passes; `AGENTS.md` conditional owners all
resolve; a new contributor can follow the first-feature guide on the scaffold.

### Stage 6: Agent harness and spec-first workflow

Goal: the harness-neutral workflow, roles, and adapters, ported with the Go
specific phase renamed.

- `docs/spec-first-workflow.md` router; `phases/` (Intake, Research,
  Specification, System/Integration Design, Rust Code/Ownership Design,
  Planning, Implementation, reviews); `interfaces/` result contracts;
  `shared/` (artifacts, evidence contract, external effects, review, resume,
  transition, cleanup, repository boundaries, read-only delegation);
  `rubrics/`.
- `docs/agent-harness.md` and adapters for Codex, Claude Code, Cursor, Qwen
  Code, Grok Build, OpenCode; `docs/prompt-composition.md`,
  `docs/prompt-maintenance.md`, `docs/skill-authoring.md`,
  `docs/subagent-brief-template.md`.
- `.agents/roles`, `.agents/role-classes`, `.agents/contracts`,
  `.agents/codex-project.toml`; generated carriers under `.claude`, `.codex`,
  `.cursor`, `.qwen`, `.grok`, `.opencode`; `scripts/agent-roles-sync.sh`,
  `scripts/codex-agents-sync.sh`, `scripts/harness-skills-sync.sh`, and their
  `--check` modes wired into CI's `agent_instructions` surface.
- `CLAUDE.md`, `QWEN.md`, `Grok.md`, `.cursor/rules/agent-harness.mdc`,
  `opencode.json`.

Exit criteria: every carrier check passes; a Cursor or Codex session can bind
`/orchestrator` and dispatch a Lead; `AGENTS.md` reaches its final shape.

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
`rust-delivery-platform` with stage 4. Remaining
for this stage: capability skills with their stages,
the harness-neutral skills and Claude/Qwen views with stage 6, universal
disciplines when a capability reaches them, and behavioural evaluation
fixtures with the reviewer roles of stage 6.

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
`merge-conflict-resolution`, `thermo-nuclear-code-quality-review`) port with
path changes only.

`docs/universal-disciplines/` is language-neutral and ports verbatim except
its "reached from" column.

Each skill follows `docs/skill-authoring.md`: description as a routing
discriminator, one decision, checkable completion, references only for
branch-only pressure. Add contrasting evaluation fixtures under `evals/` for
any skill whose decision differs from its Go or CLI source.

Exit criteria: skill catalog validates; the neighbor map has no collisions;
each new or changed skill has at least one positive and one negative fixture.

### Stage 8: PostgreSQL profile

Goal: `DATABASE=postgres` adds a pool, migrations, transactions, and proof.

- Decision spike: `sqlx` (compile-time checked SQL, offline `.sqlx` metadata,
  `sqlx migrate`) vs. alternatives; record in `specs/`.
- `crates/infra-postgres`: strict DSN admission mirroring the Go rules (URL
  form, explicit `sslmode`, no libpq environment or passfile side channels),
  pool with connection and statement timeouts, readiness probe, transaction
  helper with commit-outcome policy.
- `crates/migrate` binary with session lock, orchestration deadline, and
  append-only history check; `migrations/*.sql`.
- Integration tests through `testcontainers` behind `ALLOW_HEAVY=1` /
  `REQUIRE_DOCKER=1`; `make test-integration-db`; `make migration-validate`
  rehearsal against the runtime image.
- `docs/architecture/persistence.md`, `docs/validation/postgres.md`,
  `env/docker-compose.yml` for local PostgreSQL.

Exit criteria: the profile is inert unless selected; unit tests need no
Docker; integration proof runs in CI on the `db_integration` surface.

### Stage 9: Template initializer, profiles, and sync

Goal: `make template-init` turns the template into one service, and
`template-sync.sh` propagates portable instructions to derived repositories.

- `scripts/init-module.sh` equivalent: rewrites the service crate and binary
  name, description, `CODEOWNERS`, and `README` identity; selects profiles;
  removes unselected profile packs by marker; records `template.lock`;
  rejects unsupported combinations before mutation.
- Profile markers (`# profile:<name>:start/end`) in Cargo manifests, source,
  Dockerfile, config, OpenAPI, and docs.
- `template-owned.paths`, `scripts/template-sync.sh` with `--check`,
  `--apply`, `--instructions-only`, all refusal rules, and the purity check.
- `make template-init-check` matrix in CI on the `module_initializer` surface.
- `docs/template-sync.md`.

Exit criteria: every supported profile combination initializes, builds, and
passes `make check` in the CI matrix; a derived repository syncs cleanly.

### Stage 10: Optional capability profiles

Each profile is its own sub-stage with a guide under `docs/`, an inert default,
markers, tests, and initializer support. Order by expected demand:

1. Authentication: `AUTHN=oidc-jwt` (JWKS discovery and refresh, token
   profiles) and `AUTHN=oidc-introspection`; bearer grammar and sanitized
   failure taxonomy in `crates/infra-bearerauthn`.
2. Bounded outbound HTTP: fixed-authority `reqwest` client with post-DNS
   public-address admission, header and body ceilings, correlation stripping,
   no proxy.
3. HTTP idempotency on PostgreSQL: `x-idempotent: true` operations, replay
   evidence and business effect in one transaction.
4. Durable background jobs on PostgreSQL and the `jobs-worker` binary.
5. Outbound webhooks (Standard Webhooks signing, retry, public-address
   predicate) and inbound webhooks (verification, receipt deduplication,
   durable dispatch).
6. NATS JetStream messaging with typed domain events and a `worker` binary;
   transactional outbox with an `outbox-relay` binary.
7. gRPC with `tonic`: server policy, interceptors, health, bounded drain,
   shared client connections, buf lint and breaking checks.
8. OAuth 2.0 client-credentials outbound authentication.
9. S3-compatible object storage with one fixed endpoint.
10. `examples/reference-service`: one isolated vertical slice.

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
