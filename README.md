<h1 align="center">Rust REST API &amp; Microservice Template</h1>

<p align="center">
  An OpenAPI-first Rust service template on Tokio and axum with safe runtime defaults, optional PostgreSQL and agent-workflow profiles, observability, and CI.
</p>

<p align="center">
  <a href="https://github.com/Dankosik/rust-service-template-rest/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Dankosik/rust-service-template-rest/actions/workflows/ci.yml/badge.svg?branch=main&amp;event=push"></a>
  <a href="rust-toolchain.toml"><img alt="Rust 1.98" src="https://img.shields.io/badge/rust-1.98-B7410E?logo=rust"></a>
  <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/github/license/Dankosik/rust-service-template-rest"></a>
</p>

<p align="center">
  <a href="https://github.com/new?template_owner=Dankosik&template_name=rust-service-template-rest"><strong>Use this template</strong></a>
  ·
  <a href="#quickstart">Quickstart</a>
  ·
  <a href="docs/roadmap.md">Roadmap</a>
</p>

> **Status: stage 2 of 12.** The repository is a runnable health-only service
> with layered configuration, structured logs, OpenTelemetry traces,
> Prometheus metrics, a hardened HTTP chain, cached readiness, and a staged
> graceful shutdown. The OpenAPI contract, delivery gates, profiles, and the
> full agent harness are being ported stage by stage from
> [go-service-template-rest](https://github.com/Dankosik/go-service-template-rest).
> [The roadmap](docs/roadmap.md) is the source of truth for what exists and
> what is next.

## What this repository is

A starting point for a Rust HTTP API or microservice. It connects the pieces
most services need: layered configuration, health checks, graceful shutdown,
telemetry, a hardened HTTP server, tests, CI, and repository instructions for
coding agents; the OpenAPI contract and Docker delivery follow in the next
stages. It is a port of the *decisions* in the Go template, re-derived for
what Rust's type system and ecosystem already provide: every stage starts with
a survey of the crates that already solve the problem, and template-owned code
exists only for a documented gap.

The initialized service is small by default. Capabilities such as PostgreSQL,
jobs, messaging, gRPC, authentication, and webhooks will arrive as profiles you
select; the initializer removes everything else instead of leaving dormant code
behind.

## Why use it

- Start with a runnable service and spend the first commit on domain behavior.
- Keep the API contract, generated bindings, runtime wiring, and checks in one
  repository, with the crate graph enforcing dependency direction.
- Give people and coding agents the same ownership rules and validation paths.

## Quickstart

Requires [rustup](https://rustup.rs) and GNU Make. The pinned toolchain in
`rust-toolchain.toml` installs itself on first use.

```bash
gh repo create my-service \
  --template Dankosik/rust-service-template-rest \
  --public \
  --clone

cd my-service
make build
make test
make run
```

Then, in another terminal:

```bash
curl -i http://127.0.0.1:8080/health/live    # 200 ok
curl -i http://127.0.0.1:8080/health/ready   # 200 ok, 503 not ready while draining
curl -i http://127.0.0.1:8080/missing        # 404 application/problem+json
curl -s http://127.0.0.1:9090/metrics        # Prometheus exposition
```

`make run` loads `env/config/local.toml` (text logs, no readiness propagation
delay). Every key can be overridden with `APP__SECTION__KEY`, for example
`APP__HTTP__ADDR=:9000` or `APP__LOG__LEVEL=debug`; unknown keys and secrets
in files fail startup. `SIGINT` or `SIGTERM` flips readiness off, waits for
load balancers, drains in-flight requests, flushes telemetry, and exits `0`
(`3` when a teardown stage overran its budget). See
[Configuration Source Policy](docs/configuration-source-policy.md).

## What is here now

| Area | Included |
| --- | --- |
| Configuration | `config` crate + serde: code defaults → `--config` → `--config-overlay` → `APP__*` env; unknown keys, malformed names, and secrets in files fail; `SecretString` secrets; durations and byte sizes in human form |
| HTTP | axum router behind a `tower-http` chain: request id, `nosniff`, OpenTelemetry span, metrics, access log, `503` shedding, `504` timeout, sanitized `500`, `413`, RFC 9457 problems for `404`/`405`; hyper accept loop with header timeout, `431` header bound, connection cap |
| Readiness | Background probe refresher with failure threshold and staleness guard; `/health/ready` is a cached O(1) read; liveness is process-only |
| Observability | JSON or text logs with trace and span ids on every record; OpenTelemetry traces with OTLP/HTTP export when an endpoint is configured; Prometheus metrics (HTTP, process, Tokio runtime) on a private `:9090` listener |
| Lifecycle | Staged shutdown under one grace deadline: readiness off → propagation delay → drain → diagnostics → background join → telemetry flush; process-level tests of the built binary |
| Workspace | Pinned stable toolchain, edition 2024, workspace-level dependency versions and lints (`clippy::pedantic`, `unsafe_code = "forbid"`), committed `Cargo.lock`, `--locked` everywhere |
| Commands | `Makefile` + `make/template.mk`: `build`, `run`, `test`, `test-package`, `fmt`, `fmt-check`, `lint`, `check` |
| Delivery | GitHub Actions CI (format, clippy, build, test) with pinned action SHAs and an always-reported `required` job; Dependabot for Cargo and Actions |
| Agent workflow | `AGENTS.md` repository contract, 15 model-invoked skills under `.agents/skills`, `CLAUDE.md`, and the [roadmap](docs/roadmap.md) that names the Go-template source for every planned owner |
| Community | MIT license, code of conduct, security policy, issue forms, pull-request template, `CODEOWNERS` |

## What comes next

The [roadmap](docs/roadmap.md) decomposes the port into twelve stages with
exit criteria and a concept map from Go mechanisms to their Rust equivalents.
Next is the OpenAPI-first contract with generated bindings (stage 3), then
changed-surface CI, security gates, and the production image (stage 4).

## Repository map

```text
crates/service/             entrypoint, composition root, staged shutdown, process tests
crates/config/              typed configuration snapshot, loader, validation
crates/health/              readiness refresher, cached verdict, drain flag
crates/infra-http/          hardened middleware chain, problem details, bounded server
crates/infra-telemetry/     subscriber, tracer provider, metrics, diagnostics router
crates/<feature>/           business behavior (created with the first feature)
crates/infra-<provider>/    database, messaging, and provider adapters (per profile)
.agents/skills/             model-invoked skills encoding this repository's decisions
env/config/local.toml       local baseline configuration
docs/roadmap.md             stages, fixed decisions, Go-to-Rust concept map
specs/<topic>/research/     library research behind the current stage
make/template.mk            portable standard Make commands
.github/workflows/ci.yml    the source of truth for CI check names
```

## Everyday commands

| Command | Use it for |
| --- | --- |
| `make run` | Start the service with `env/config/local.toml` |
| `make build` | Build every workspace crate |
| `make test` | Run the workspace unit-test suite |
| `make test-package PKG=<crate>` | Run one crate's tests |
| `make fmt` / `make fmt-check` | Format, or fail on unformatted code |
| `make lint` | Clippy over all targets with warnings as errors |
| `make check-skills` | Validate the shape of `.agents/skills` |
| `make check` | Full local gate: `fmt-check`, `lint`, `test`, `check-skills` |

Stop at the local completion criterion in
[AGENTS.md](AGENTS.md#validation-budget) rather than adding checks for
confidence.

## Working with coding agents

`AGENTS.md` gives every supported agent the repository rules: authority,
decision ownership, engineering constraints, the validation budget, and the
crate-boundary model. `.agents/skills` holds focused, model-invoked skills
that encode this repository's decisions; Cursor, Codex, Grok, and OpenCode
read them directly, and the Claude Code and Qwen views arrive with the
harness stage. Each skill names the repository owner it decides against, so
an agent extends the existing path instead of creating a parallel one.

| Skill | Leading concept | Use it for |
| --- | --- | --- |
| [rust-coder](.agents/skills/rust-coder/SKILL.md) | Earliest owner | Implementing an authorized change with its tests and cleanup |
| [rust-idiomatic](.agents/skills/rust-idiomatic/SKILL.md) | Contracts | Ownership, borrowing, trait bounds, error identity, `Send`/`Sync` boundaries |
| [rust-tokio](.agents/skills/rust-tokio/SKILL.md) | Task ownership | Spawning, `select!`, locks across awaits, channels, blocking work, shutdown order |
| [rust-axum](.agents/skills/rust-axum/SKILL.md) | Route tree | Routes, extractors, layers, fallbacks, the hardened chain |
| [rust-errors](.agents/skills/rust-errors/SKILL.md) | Failure semantics | Typed errors, problem codes, exit codes, fail vs degrade |
| [rust-config](.agents/skills/rust-config/SKILL.md) | One key, one owner | Adding or validating a configuration key or secret source |
| [rust-observability](.agents/skills/rust-observability/SKILL.md) | Operator evidence | Log fields, spans, metrics, probes, cardinality, privacy |
| [rust-reliability](.agents/skills/rust-reliability/SKILL.md) | Budget arithmetic | Deadlines, retries, overload, readiness, drain, shutdown budgets |
| [rust-security](.agents/skills/rust-security/SKILL.md) | Attacker path | Identity, secrets, caller input, exposure, amplification |
| [rust-testing](.agents/skills/rust-testing/SKILL.md) | Observable failure | Proving layer, deterministic time, process tests |
| [rust-performance](.agents/skills/rust-performance/SKILL.md) | Evidence | Latency, throughput, allocation, allocator and profile decisions |
| [rust-debugging](.agents/skills/rust-debugging/SKILL.md) | Causality | Failures, hangs, flakes, wrong output |
| [rust-structural-quality](.agents/skills/rust-structural-quality/SKILL.md) | Deletion test | New crates, modules, traits, layers, helpers, placement |
| [rust-dependencies](.agents/skills/rust-dependencies/SKILL.md) | Verified resolution | New crates or features, toolchain pin, library vs template code |
| [rust-verification](.agents/skills/rust-verification/SKILL.md) | Evidence boundary | What existing evidence supports a claim |

Skills for a capability arrive with its stage (`rust-api-contract`,
`rust-sqlx`, `rust-tonic`, delivery). Authoring rules and the structural
check live in [Skill Authoring](docs/skill-authoring.md) and
`make check-skills`. The set builds on
[rust-cli-skills](https://github.com/Dankosik/rust-cli-skills) and the Go
template's `go-*` skills where their decisions carry over.

## Documentation

- Plan and status: [Roadmap](docs/roadmap.md)
- Configuration, secrets, telemetry environment, runtime budgets: [Configuration Source Policy](docs/configuration-source-policy.md)
- Writing skills: [Skill Authoring](docs/skill-authoring.md)
- Contributing and validation: [CONTRIBUTING.md](CONTRIBUTING.md)
- Agent contract: [AGENTS.md](AGENTS.md)

## Community

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md), use the
issue forms for bugs and feature proposals, and follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

Report vulnerabilities privately through [SECURITY.md](SECURITY.md).

Released under the [MIT License](LICENSE).
