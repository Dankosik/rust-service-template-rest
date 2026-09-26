# rust-service-template-rest

OpenAPI-first Rust HTTP service on Tokio and axum.

Repository: https://github.com/Dankosik/rust-service-template-rest

The HTTP core includes typed configuration, health and readiness, structured
logs and telemetry, bounded request handling, joined graceful shutdown, a
committed OpenAPI contract generated from Rust, and surface-selected CI.
The local architecture and deployment policy remain owned by this repository.

Upstream template provenance:
[rust-service-template-rest](https://github.com/Dankosik/rust-service-template-rest),
which ports decisions from
[go-service-template-rest](https://github.com/Dankosik/go-service-template-rest).
These links describe origin, not the current service's owners or support route.

## Quickstart

Install Git, Python 3.11+, GNU Make and rustup. Use the pinned toolchain in
`rust-toolchain.toml`; Cargo commands use the committed lockfile.

```sh
make build
make test
make run
```

`make run` uses `env/config/local.toml`. The default service needs no external
services. Check `http://127.0.0.1:8080/health/live` and `/health/ready`;
Prometheus metrics use the separate listener at `http://127.0.0.1:9090/metrics`.
`APP__SECTION__KEY` overrides configuration. Unknown keys and secrets in files
fail startup. A stop signal drains admitted work, joins background tasks and
flushes telemetry. [Configuration](docs/configuration-source-policy.md) and
[Runtime Lifecycle](docs/architecture/runtime-lifecycle.md) own the details.

## Initialize a fresh template checkout

A clean tracked checkout selects database, optional capability profiles, and an
agent harness. `DATABASE` defaults to `none`; `AGENT_HARNESS` defaults to `all`
and accepts `core`, `codex`, `claude`, `qwen`, `cursor`, `grok`, `opencode`, or
`all`. Capability profiles are documented with their owning guides.

```sh
make template-init SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' CODEOWNER=@example/platform \
  DATABASE=none AGENT_HARNESS=claude
```

Initialization changes package and executable identity, configuration defaults,
OpenAPI, owners and local command data, and removes unselected packs. It keeps
the stable `crates/service` directory and library name. `template.lock` records
local source provenance and selected profiles. Exact replay is a structural
no-op; changing an established profile is a separate operation. The command
never stages or commits the result. Review and commit the initialized diff.
[Initialization and portable updates](docs/template-sync.md) owns admission,
prerequisites, refusal and recovery.

## Profiles and local owners

<!-- template:begin grpc:readme-grpc-profile -->
`GRPC=enabled` retains native tonic/prost RPCs on a separate HTTP/2 listener,
generated contracts, standard health, shared lazy clients and bounded shutdown.
It is inert until explicitly enabled and registered. `GRPC=none` removes the
complete capability. The [gRPC guide](docs/grpc.md) owns registration, validation,
TLS/plaintext trust, authentication and generation; the [decision
record](docs/grpc-decisions.md) explains the library boundaries.
<!-- template:end grpc:readme-grpc-profile -->

<!-- template:begin webhooks-common:readme-webhooks-profiles -->
The optional webhook profiles are selected by `WEBHOOKS=none|durable` and
`INBOUND_WEBHOOKS=none|standard-webhooks`, both defaulting to `none`. Each
requires PostgreSQL and jobs; durable outbound additionally requires bounded
outbound HTTP. Retention is inert until static environment-backed endpoints and,
for inbound processing, explicit service/worker consumer bindings exist. See
the selected direction guide.
<!-- template:end webhooks-common:readme-webhooks-profiles -->
<!-- template:begin messaging:docs-readme-messaging-profile -->
`MESSAGING=nats-jetstream` retains typed JetStream publication and consumption
without PostgreSQL or jobs. It is inert until configured and reuses the retained
`jobs-worker`; it does not add a sample event or HTTP route. The [durable
messaging guide](docs/durable-messaging.md) owns Go interoperability, operator
topology, handler idempotency, and the selected worker's limits. `OUTBOX` is a
separate PostgreSQL/jobs extension and is unavailable in a messaging-only
selection.
<!-- template:end messaging:docs-readme-messaging-profile -->
<!-- template:begin outbox:docs-readme-outbox-profile -->
`OUTBOX=postgres` requires `DATABASE=postgres`, `JOBS=postgres`, and
`MESSAGING=nats-jetstream`. It records publication intent in the business
transaction and gives it reserved worker capacity. Its recovery and rollback
rules are in the [transactional outbox guide](docs/postgres-transactional-outbox.md).
<!-- template:end outbox:docs-readme-outbox-profile -->

<!-- template:begin webhooks:readme-webhooks-outbound-guide -->
The outbound direction is documented in
[Outbound webhooks](docs/outbound-webhooks.md).
<!-- template:end webhooks:readme-webhooks-outbound-guide -->

<!-- template:begin inbound-webhooks:readme-webhooks-inbound-guide -->
The inbound direction is documented in
[Inbound webhooks](docs/inbound-webhooks.md).
<!-- template:end inbound-webhooks:readme-webhooks-inbound-guide -->

<!-- template:begin outbound-auth:readme-outbound-auth-profile -->
The optional outbound machine-authentication profile is selected by
`OUTBOUND_AUTH=none|oauth2-client-credentials`, defaulting to `none`.
Selecting OAuth2 also retains bounded outbound HTTP, but starts no provider
call, task, listener, or readiness dependency. Concrete integrations compose
their own private authenticated client. See [Outbound machine
authentication](docs/outbound-machine-authentication.md) and its [decision
record](docs/outbound-machine-authentication-decisions.md).
<!-- template:end outbound-auth:readme-outbound-auth-profile -->

The database and installed adapters are selected by `template.lock`; the source
checkout without a lock carries PostgreSQL and the optional adapters. A retained
PostgreSQL profile remains inert until configured. The
[local persistence record](docs/architecture/persistence.md) describes its
availability, and [PostgreSQL Validation](docs/validation/postgres.md) names
proof only when that profile exists. An absent profile contributes no provider,
migrator, configuration, database tests or executable database gate.

`make/service.mk` owns the service package/bin, image tag, configuration and
OpenAPI paths, and local recipes. `make/template.mk` owns the portable standard
commands. `make help` lists the actual selected surface; the
[command policy](docs/build-test-and-development-commands.md) names its proof
boundaries. Ordinary development finishes after the matching build and tests;
`ALLOW_FULL=1 make check` is the explicit aggregate. Container-backed work
additionally requires `ALLOW_HEAVY=1`.

The image uses the pinned Dockerfile and the local image default:

```sh
ALLOW_HEAVY=1 make runtime-image-build
```

Image execution, scanning and publication require the corresponding accepted
claim and authority. [CI/CD Production Readiness](docs/ci-cd-production-ready.md)
and [Railway Deployment Profile](docs/railway-deployment-profile.md) retain the
local gate and rollout decisions. Initialization does not deploy a service.

## Portable updates

Use one committed template checkout as the source and this service as target:

```sh
scripts/template-sync.sh --check --from /path/to/template --repo .
scripts/template-sync.sh --apply --from /path/to/template --repo .
```

The ownership manifest selects portable files. Runtime/Cargo sources, local
configuration, OpenAPI, migrations, README, owners, CI activation and architecture
remain service-owned. `--instructions-only` adopts selected instruction and
adapter views while leaving tooling untouched. Commit adopted changes before
checking parity; dirty owned paths refuse even when their bytes already match.
See [synchronization](docs/template-sync.md) for local skills, managed settings
leaves, selective dirty refusal and recovery.

## Working with coding agents

[AGENTS.md](AGENTS.md) owns authority, engineering and local completion.
[Workflow Router](docs/spec-first-workflow.md) selects the phase for non-direct
work; [Agent Harness](docs/agent-harness.md) describes native delegation.
Canonical skills and roles remain under `.agents` for every selection. Only
adapters selected in `template.lock` are installed and checked by
`make check-instructions`. Product-specific commands refuse when unselected;
`core` uses canonical instructions without product views. The adapter documents
remain reference guidance for each product, not declarations of installation.

| Skill | Leading concept | Use it for |
| --- | --- | --- |
| [rust-coder](.agents/skills/rust-coder/SKILL.md) | Earliest owner | Implementing an authorized change with its tests and cleanup |
| [rust-idiomatic](.agents/skills/rust-idiomatic/SKILL.md) | Contracts | Ownership, borrowing, trait bounds, error identity, `Send`/`Sync` boundaries |
| [rust-tokio](.agents/skills/rust-tokio/SKILL.md) | Task ownership | Spawning, `select!`, locks across awaits, channels, blocking work, shutdown order |
| [rust-axum](.agents/skills/rust-axum/SKILL.md) | Route tree | Routes, extractors, layers, fallbacks, the hardened chain |
| [rust-api-contract](.agents/skills/rust-api-contract/SKILL.md) | Observable contract | OpenAPI operations, security decisions, problem responses, drift and compatibility |
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
| [rust-delivery-platform](.agents/skills/rust-delivery-platform/SKILL.md) | Gate chain | CI jobs, tool pins, the Dockerfile, image checks, publication, deployment profile |
| [rust-sqlx](.agents/skills/rust-sqlx/SKILL.md) | Durable outcome | PostgreSQL pool, DSN admission, transactions, commit outcome, migrations, database-backed proof |
| [merge-conflict-resolution](.agents/skills/merge-conflict-resolution/SKILL.md) | Intent reconstruction | Conflicted hunks in a merge, rebase, cherry-pick, or revert |

Capability methods apply only when the local architecture provides their
capability. [Skill Authoring](docs/skill-authoring.md) owns their shape and
[Prompt Maintenance](docs/prompt-maintenance.md) owns instruction changes.

## Documentation

- [Repository Architecture](docs/repo-architecture.md) and [Component Boundaries](docs/architecture/boundaries.md).
- [HTTP Architecture](docs/architecture/http.md), [Integration Boundaries](docs/architecture/integration.md), and [Project Structure](docs/project-structure-and-module-organization.md).
- [First Production Feature](docs/first-production-feature.md) and [Production Contract](docs/production-contract.md).
- [Validation Routing](docs/validation-routing.md), [Commands](docs/build-test-and-development-commands.md), and [CONTRIBUTING.md](CONTRIBUTING.md).
- [Backend Library Selection](docs/backend-library-selection.md) and [Backend Utility Recipes](docs/backend-utility-recipes.md).
- [Prompt Composition](docs/prompt-composition.md) and [Initialization and portable updates](docs/template-sync.md).

## Community

Use the repository issue forms and follow the [Code of Conduct](CODE_OF_CONDUCT.md).
Report vulnerabilities privately through [SECURITY.md](SECURITY.md).
Released under the [MIT License](LICENSE).
