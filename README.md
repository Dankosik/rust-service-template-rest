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

> **Status: stage 1 of 12.** The repository is a runnable health-only scaffold
> with the toolchain, workspace layout, lint policy, command surface, CI, and
> agent contract in place. Configuration, telemetry, the OpenAPI contract,
> profiles, and the full agent harness are being ported stage by stage from
> [go-service-template-rest](https://github.com/Dankosik/go-service-template-rest).
> [The roadmap](docs/roadmap.md) is the source of truth for what exists and
> what is next.

## What this repository is

A starting point for a Rust HTTP API or microservice. It will connect the
pieces most services need: an OpenAPI contract, layered configuration, health
checks, graceful shutdown, telemetry, tests, Docker, CI, and repository
instructions for coding agents. It is a port of the *decisions* in the Go
template, re-derived for what Rust's type system and ecosystem already provide.

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
```

`APP__HTTP__ADDR` overrides the listen address (`host:port` or Go-style
`:port`; default `0.0.0.0:8080`). `RUST_LOG` filters log output (default
`info`). `SIGINT` or `SIGTERM` flips readiness off, drains in-flight requests
inside a 25 s budget, and exits `0`.

## What is here now

| Area | Included |
| --- | --- |
| Runtime | Tokio multi-thread runtime, axum router, `/health/live` and `/health/ready`, shared readiness flag, bounded graceful shutdown on `SIGINT`/`SIGTERM` |
| Workspace | Pinned stable toolchain, edition 2024, workspace-level dependency versions and lints (`clippy::pedantic`, `unsafe_code = "forbid"`), committed `Cargo.lock`, `--locked` everywhere |
| Commands | `Makefile` + `make/template.mk`: `build`, `run`, `test`, `test-package`, `fmt`, `fmt-check`, `lint`, `check` |
| Delivery | GitHub Actions CI (format, clippy, build, test) with pinned action SHAs and an always-reported `required` job; Dependabot for Cargo and Actions |
| Agent workflow | `AGENTS.md` repository contract, `CLAUDE.md`, and the [roadmap](docs/roadmap.md) that names the Go-template source for every planned owner |
| Community | MIT license, code of conduct, security policy, issue forms, pull-request template, `CODEOWNERS` |

## What comes next

The [roadmap](docs/roadmap.md) decomposes the port into twelve stages with
exit criteria and a concept map from Go mechanisms to their Rust equivalents.
The next two stages are independent: layered configuration, structured logs,
OpenTelemetry, and the hardened HTTP chain (stage 2), and the OpenAPI-first
contract with generated bindings (stage 3).

## Repository map

```text
crates/service/             service entrypoint and runtime composition root
crates/infra-http/          HTTP adapter: router, health probes, server lifecycle
crates/<feature>/           business behavior (created with the first feature)
crates/infra-<provider>/    database, messaging, and provider adapters (per profile)
docs/roadmap.md             stages, fixed decisions, Go-to-Rust concept map
make/template.mk            portable standard Make commands
make/service.mk             optional service-owned Make extensions
.github/workflows/ci.yml    the source of truth for CI check names
```

## Everyday commands

| Command | Use it for |
| --- | --- |
| `make run` | Start the HTTP service locally |
| `make build` | Build every workspace crate |
| `make test` | Run the workspace unit-test suite |
| `make test-package PKG=<crate>` | Run one crate's tests |
| `make fmt` / `make fmt-check` | Format, or fail on unformatted code |
| `make lint` | Clippy over all targets with warnings as errors |
| `make check` | Full local gate: `fmt-check`, `lint`, `test` |

Stop at the local completion criterion in
[AGENTS.md](AGENTS.md#validation-budget) rather than adding checks for
confidence.

## Working with coding agents

`AGENTS.md` gives every supported agent the repository rules: authority,
decision ownership, engineering constraints, the validation budget, and the
crate-boundary model. Focused Rust skills, harness adapters for Codex, Claude
Code, Cursor, Qwen Code, Grok Build, and OpenCode, and the spec-first workflow
arrive with stages 6 and 7; the skills build on
[rust-cli-skills](https://github.com/Dankosik/rust-cli-skills) where its
decisions carry over to a long-running service.

## Documentation

- Plan and status: [Roadmap](docs/roadmap.md)
- Contributing and validation: [CONTRIBUTING.md](CONTRIBUTING.md)
- Agent contract: [AGENTS.md](AGENTS.md)

## Community

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md), use the
issue forms for bugs and feature proposals, and follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

Report vulnerabilities privately through [SECURITY.md](SECURITY.md).

Released under the [MIT License](LICENSE).
