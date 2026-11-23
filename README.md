<!-- template:begin source-template:readme-banner -->
<p align="center">
  <img src="docs/assets/banner.png" alt="rust-service-template-rest: a crab with a wrench and a laptop" width="760">
</p>

<p align="center">
  <a href="#start-a-service">Start a service</a> ·
  <a href="#how-agents-work-here">How agents work here</a>
</p>
<!-- template:end source-template:readme-banner -->

# rust-service-template-rest

Rust REST API service template on Tokio and axum, built to be developed by coding agents.

Repository: https://github.com/Dankosik/rust-service-template-rest

<!-- template:begin source-template:readme-template-overview -->
[![CI](https://github.com/Dankosik/rust-service-template-rest/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Dankosik/rust-service-template-rest/actions/workflows/ci.yml)
[![Rust toolchain](https://img.shields.io/badge/rust-pinned-orange.svg)](rust-toolchain.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Coding agents now write most of a service's code, and they repeat whatever the
repository makes easy. This template makes the production path the easy one.
The decisions a Rust microservice needs are already made, enforced by tests,
lints and CI, and written as instructions the agent follows. You describe the
behavior. The agent builds it inside fixed boundaries and proves it before
merge.

Works with Claude Code, Codex, Cursor, Qwen Code, Grok, OpenCode and Antigravity.

## Start a service

1. Create a repository with **Use this template**.
2. Initialize it once: ask your agent, or run

   ```sh
   cargo fetch --locked
   just template-init --service-name catalog-api \
     --repository https://github.com/example/catalog-api \
     --description 'Catalog API' --codeowner @example/platform \
     --agent-harness claude
   ```

   The initializer renames the service, keeps the profiles you select, and
   deletes the rest. It never commits, so you review the diff first.
   [Initialization](docs/template-init.md) lists every option.
3. Describe the first feature to your agent.
<!-- template:end source-template:readme-template-overview -->

## How agents work here

- [AGENTS.md](AGENTS.md) is the contract: what the agent decides on its own,
  what it asks you, and how much proof finishes a change.
- [Skills](.agents/skills) give one focused method per concern: Tokio tasks,
  axum routes, the API contract, errors, configuration, observability,
  reliability, security, SQL, tests, performance and dependencies.
- Larger work follows a [spec-first workflow](docs/spec-first-workflow.md):
  intake, specification, technical design, planning, implementation, and review.
- Lints, tests and CI hold the agent to the same rules as a person. CI runs
  only the checks a change touches.
- One canonical instruction set lives in `.agents`. Initialization installs only
  the adapters `AGENT_HARNESS` selects.

<!-- template:begin source-template:readme-harness-sync -->
This template repository does not commit the generated agent files
(`CLAUDE.md`, `.claude/`, `.agents/agents/` and the other harness folders).
After you clone or pull it, run `just agent-harness-sync` so your agent finds
its instructions and subagents. To change a subagent, edit `.agents/roles` or
`.agents/role-classes`, then run the same command; the next sync overwrites
direct edits to generated files. A service created from the template commits
these files, so this step is not needed there.
<!-- template:end source-template:readme-harness-sync -->

## What the service includes

- An axum HTTP server with connection, body and in-flight limits, `503` load
  shedding, a request deadline and panic recovery.
- OpenAPI generated from Rust and committed. CI fails on drift and on
  unapproved breaking changes.
- Application errors use `application/problem+json` from a closed catalog.
  Readiness reports an unavailable instance with `503 text/plain`.
- Health and readiness probes, JSON logs, OpenTelemetry traces, Prometheus
  metrics on a private listener, and [SLO burn-rate alerts](docs/alerting.md)
  with runbooks.
- Graceful shutdown, typed configuration, and a distroless image with a scan
  and an SBOM.

## Profiles

Optional capabilities are chosen once at initialization. Unselected ones are
deleted; selected ones stay inert until configured.

<!-- template:begin postgres:readme-postgres-profile -->
- `DATABASE=postgres`: PostgreSQL through sqlx, with migrations and
  compile-time checked queries ([`infra-postgres`](crates/infra-postgres)).
<!-- template:end postgres:readme-postgres-profile -->
<!-- template:begin jobs:readme-jobs-profile -->
- `JOBS=postgres`: durable background jobs and a worker binary
  ([`infra-jobs`](crates/infra-jobs)).
<!-- template:end jobs:readme-jobs-profile -->
<!-- template:begin http-idempotency:readme-http-idempotency-profile -->
- `HTTP_IDEMPOTENCY=postgres` enables `Idempotency-Key` on opted-in operations.
  The client retry window is one minute from the first send unless the service
  publishes a longer window. The service must check the key before the window
  ends. Allow for transit, request processing and database queueing. If there
  may be too little time left, check whether the original operation took
  effect before sending it again.
  [PostgreSQL idempotency store](crates/infra-idempotency-store)
<!-- template:end http-idempotency:readme-http-idempotency-profile -->
<!-- template:begin authn:readme-authn-profile -->
- `AUTHN=oidc-jwt|oidc-introspection`: bearer authentication against an OIDC
  provider ([`infra-bearerauthn`](crates/infra-bearerauthn)).
<!-- template:end authn:readme-authn-profile -->
<!-- template:begin oidc-jwt:readme-jwt-migration -->
  For a new JWT service, pass `--config-overlay env/config/oidc-jwt.toml` and
  supply the issuer, API audience and `authn.jwks_max_age` for the deployment.
  The overlay selects RFC 9068 access tokens. When migrating an existing JWT
  service, set `authn.token_profile` explicitly. An omitted profile previously
  selected `resource-server`. Qualify the issuer and choose the key-revocation
  window before rolling out the new binary. See the
  [production contract](docs/production-contract.md#edge-and-trust).
<!-- template:end oidc-jwt:readme-jwt-migration -->
<!-- template:begin outbound-http:readme-outbound-http-profile -->
- `OUTBOUND_HTTP=bounded`: an HTTPS client with budgets
  ([`infra-outbound-http`](crates/infra-outbound-http)).
<!-- template:end outbound-http:readme-outbound-http-profile -->
<!-- template:begin outbound-auth:readme-outbound-auth-profile -->
- `OUTBOUND_AUTH=oauth2-client-credentials`: OAuth2 machine authentication with
  a private key and RFC 8693 token exchange
  ([`infra-oauth2-client-credentials`](crates/infra-oauth2-client-credentials)).
  To call a provider as the service, pass `ServiceRegistration::for_http_resource(prepare)`
  to `service::run_with`. In `prepare`,
  pass the OAuth registration and target to `background.service_tokens` and return the adapter
  with its authenticated transport. Keep response limits, timeouts and provider
  error mapping in that adapter. In the handler, extract `Extension<MyAdapter>`
  and `RequestContext`, then pass `ctx.operation()` to HTTP `.execute(...)`
  calls (or bind a gRPC client with `.with_context(ctx.operation())`).
  The process drives and joins token refresh.
<!-- template:end outbound-auth:readme-outbound-auth-profile -->
<!-- template:begin grpc:readme-grpc-profile -->
- `GRPC=enabled`: tonic gRPC on a separate listener and an outbound client
  ([`infra-grpc`](crates/infra-grpc), [`infra-outbound-grpc`](crates/infra-outbound-grpc)).
<!-- template:end grpc:readme-grpc-profile -->
<!-- template:begin service-auth:docs-readme-service-auth-profile -->
- `SERVICE_AUTH=jwt`: signed service-to-service tokens for gRPC
  ([`infra-serviceauth`](crates/infra-serviceauth)).
<!-- template:end service-auth:docs-readme-service-auth-profile -->
<!-- template:begin messaging:docs-readme-messaging-profile -->
- `MESSAGING=nats-jetstream`: typed events on NATS JetStream
  ([`infra-messaging`](crates/infra-messaging)).
<!-- template:end messaging:docs-readme-messaging-profile -->
<!-- template:begin outbox:docs-readme-outbox-profile -->
- `OUTBOX=postgres`: a transactional outbox to JetStream
  (in [`infra-messaging`](crates/infra-messaging)).
<!-- template:end outbox:docs-readme-outbox-profile -->
<!-- template:begin webhooks:readme-webhooks-outbound-guide -->
- `WEBHOOKS=durable`: signed outbound webhooks
  ([`infra-webhooks`](crates/infra-webhooks)).
<!-- template:end webhooks:readme-webhooks-outbound-guide -->
<!-- template:begin inbound-webhooks:readme-webhooks-inbound-guide -->
- `INBOUND_WEBHOOKS=standard-webhooks`: verified inbound webhooks
  ([`infra-webhooks`](crates/infra-webhooks)).
<!-- template:end inbound-webhooks:readme-webhooks-inbound-guide -->
<!-- template:begin cache:docs-readme-cache-profile -->
- `CACHE=redis`: an optional Valkey cache. Bind a source-specific time reserve
  at startup with `CacheLookup` and let the feature read from its source when
  lookup returns no value.
  Configure entry sizes and grant the read commands before rolling out the
  [cache profile](docs/decisions/cache-decisions.md).
<!-- template:end cache:docs-readme-cache-profile -->
<!-- template:begin object-storage:docs-readme-object-storage-profile -->
- `OBJECT_STORAGE=s3` supports Amazon S3, Cloudflare R2 and Railway Buckets.
  File routes use the [HTTP upload and download helpers](crates/infra-http/src/object_storage.rs)
  with the configured [storage client](crates/infra-object-storage).
<!-- template:end object-storage:docs-readme-object-storage-profile -->
<!-- template:begin clickhouse:docs-readme-clickhouse-profile -->
- `CLICKHOUSE=http`: an optional hardened ClickHouse OLAP client
  ([`infra-clickhouse`](crates/infra-clickhouse)) with bounded reads,
  explicit batched and async writes, `password_file` rotation, and
  [decision and rollout guidance](docs/decisions/clickhouse-decisions.md).
<!-- template:end clickhouse:docs-readme-clickhouse-profile -->
<!-- template:begin temporal:docs-readme-temporal-profile -->
- `TEMPORAL=sdk`: durable workflows and heartbeating activities over Temporal
  gRPC ([`infra-temporal`](crates/infra-temporal)), with a dedicated
  [`temporal-worker`](crates/temporal-worker) binary and
  [Temporal decisions](docs/decisions/temporal-decisions.md).
<!-- template:end temporal:docs-readme-temporal-profile -->

## Run it

Needs Git, Python 3.11+, [mise](https://mise.jdx.dev) (installs `just` and all pinned CLIs via `mise install`), and rustup.

```sh
mise install
just build && just test && just run
curl http://127.0.0.1:8080/health/ready
```

`just` lists every command.

## Documentation

Code is the source of truth: the committed OpenAPI document in `api/openapi/`,
typed configuration in `crates/config`, SQL migrations in `migrations/`, CI
workflows in `.github/workflows/`, `mise.toml`, and `justfile`. Markdown records only
policies, runbooks, and non-code decision reasons:

[Architecture](docs/repo-architecture.md) ·
[Production contract](docs/production-contract.md) ·
[Validation](docs/validation-routing.md) ·
[CI/CD](docs/ci-cd-production-ready.md) ·
[Deployment](docs/railway-deployment-profile.md) ·
[Alerting](docs/alerting.md)

## Community

Use the issue forms, follow the [Code of Conduct](CODE_OF_CONDUCT.md), and
report vulnerabilities through [SECURITY.md](SECURITY.md). MIT
[License](LICENSE). Template provenance:
[rust-service-template-rest](https://github.com/Dankosik/rust-service-template-rest),
ported from [go-service-template-rest](https://github.com/Dankosik/go-service-template-rest).
