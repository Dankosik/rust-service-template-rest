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
| 3 | OpenAPI-first contract and generated bindings | planned |
| 4 | Validation routing and delivery: CI surfaces, security gates, image, publication | planned |
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
| `tools/go.mod` pinned developer tools | Pinned `cargo install --locked` versions declared in one place (stage 4) | Candidates: `cargo-deny`, `cargo-nextest`, `cargo-audit`, `cargo-machete`. |
| `cmd/service/main.go` → `bootstrap.Run` | `crates/service/src/main.rs` → `bootstrap::run` | Runtime construction, signals, and drain live in `bootstrap`; `main` only maps the result to an exit code. |
| `internal/<feature>` | `crates/<feature>` | Not created until the first feature exists. |
| `internal/infra/http` (`chi`, `net/http` server) | `crates/infra-http` (axum `Router`, `tower-http` layers, hand-rolled hyper accept loop) | Middleware order is the route tree's observable semantics in both. `axum::serve` exposes no connection limits and sets no timer, so the accept loop is template-owned. |
| `internal/config` (koanf, YAML + `APP__` env + flags) | `crates/config` (`config` crate + serde, TOML files, same precedence and secret rules) | `figment` was rejected: no release since 2024, silently drops malformed env names. |
| `slog` + `logctx` | `tracing` + `tracing-subscriber` + `json-subscriber` | Typed `log.level` directive and `log.format`; `RUST_LOG` is not read. |
| OpenTelemetry Go traces and metrics + Prometheus | Traces: `opentelemetry` 0.32 + `tracing-opentelemetry` + `axum-tracing-opentelemetry`. Metrics: the `metrics` facade + `metrics-exporter-prometheus` + `axum-prometheus` + `metrics-process` + `tokio-metrics` | The facade is the Rust idiom; OTLP metric push is deferred. Diagnostics stay on a separate listener (`:9090`). |
| RFC 9457 `problem` package | `infra_http::problem` | Closed transport catalog, stable codes, no submitted values echoed; a `failure` leaf splits out with the gRPC profile. |
| oapi-codegen strict server | Decision in stage 3 | Options: spec-first generation (`openapi-generator`, `progenitor`-style) vs. code-first (`utoipa`) with a drift check against the committed spec. The template is spec-first; code-first is acceptable only if the committed spec remains the reviewed authority. |
| `pgx` + `sqlc` + Goose | Decision in stage 8 | `sqlx` with compile-time checked queries and offline metadata is the leading candidate; migrations via `sqlx migrate` or `refinery`. |
| River jobs | Decision in stage 10 | Candidates: `apalis`, `underway`, or a template-owned PostgreSQL queue. |
| NATS JetStream (`nats.go`) | `async-nats` | |
| gRPC (`grpc-go`, buf) | `tonic` + `prost`, buf for lint and breaking checks | |
| `golangci-lint`, `depguard` | clippy workspace lints; crate graph for direction; `cargo-deny` bans for forbidden crates | |
| `gosec`, `govulncheck` | `cargo-deny advisories`, `cargo-audit`; CodeQL for Rust when available on the repository | |
| `goleak`, `-race` | Ownership and `Send`/`Sync` remove data races at compile time; task completion is proven by joining; `loom` only for hand-written lock-free code | Do not port a race detector step. |
| Distroless static image | Multi-stage build; `distroless/cc` (glibc) or `distroless/static` (musl) decided in stage 4 with measured image size and build time | |
| `.agents/skills/go-*` | `.agents/skills/rust-*` (stage 7) | See the skills plan below. |

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

Research and decisions: `specs/runtime-core/research/synthesis.md` and the
four lane reports beside it. The bundle stays open until the architecture
documents of stage 5 exist to receive its durable decisions.

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

### Stage 3: OpenAPI-first contract

Goal: `api/openapi/service.yaml` is the reviewed source of truth for the HTTP
contract, and handwritten handlers implement generated request and response
types.

- Port the health-only `service.yaml` (OpenAPI 3.0.3, `Problem` and
  `InvalidParam` schemas, `x-security-decision`, bearer scheme behind a
  profile marker).
- Research spike, recorded in `specs/`: pick the generation strategy (see the
  concept map). Requirements: typed request extraction, typed responses per
  status, a compile-time or CI drift check between spec and code, and no
  hand-edited generated output.
- `make openapi-generate`, `make openapi-check` (drift), Redocly lint and
  validation, breaking-change comparison against the pull-request base.
- Runtime contract tests: every operation declares its security decision and
  the required problem responses.
- `docs/architecture/http.md` describing the request path from listener to
  feature handler and back.

Exit criteria: regenerating from the committed spec produces no diff; a
deliberately changed spec fails `openapi-check`; the health probes are served
through the generated interface.

### Stage 4: Validation routing and delivery

Goal: CI selects checks from the changed surfaces, security gates run where
they observe something, and a production image exists.

- `scripts/ci/changed-surfaces.sh` classifying Rust source, dependencies,
  lint config, OpenAPI, workflows, shell, Dockerfile, documentation, and agent
  instructions; `required` job accepts deliberate skips.
- `cargo-deny` (advisories, licenses, bans, sources), `cargo-audit`,
  Dependency Review on pull requests, Gitleaks with the same range/history
  policy, `actionlint`, `shellcheck`, CodeQL for Rust if the repository can
  enable it.
- Pinned developer tool versions in one manifest consumed by `make` and CI.
- `build/docker/Dockerfile`: multi-stage, reproducible (`SOURCE_DATE_EPOCH`),
  non-root, `STOPSIGNAL SIGTERM`, version and commit baked in; runtime image
  lifecycle check (start, readiness, version, clean `SIGTERM`).
- `cd.yml` with opt-in GHCR publication: run-scoped candidate, vulnerability
  scan, CycloneDX SBOM, cosign signing and attestation, verification before
  tag promotion. `railway.toml` and the deployment profile doc.
- Expanded `make/template.mk`: `verify` plan/receipt model, `ALLOW_FULL` and
  `ALLOW_HEAVY` guards, `lint-changed`, `test-changed` using
  `cargo` package selection from changed paths.
- `docs/validation-routing.md`, `docs/validation/*.md`,
  `docs/ci-cd-production-ready.md`.

Exit criteria: a docs-only pull request runs no Rust job; a Dockerfile change
builds and lifecycle-checks the image; `make verify` prints a plan and a
receipt; secret and dependency gates fail on planted findings in a test branch.

### Stage 5: Repository documentation

Goal: the same documentation graph as the Go template, rewritten for the
crate layout, with every link resolving.

- `docs/repo-architecture.md` front door with global invariants, source of
  truth table, and one-leaf selector.
- `docs/architecture/boundaries.md`, `runtime-lifecycle.md`, `http.md`,
  `integration.md`, `persistence.md` (after stage 8), `async.md` (after the
  first async profile).
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
routing to the catalog. Remaining for this stage: capability skills with their stages,
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
