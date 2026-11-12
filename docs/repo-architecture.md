# Repository Architecture

This is the architecture front door for the service identified by
`crates/service/Cargo.toml`. It records only the rules the code cannot state
by itself. Code, the committed contract, tests, and crate documentation are
the factual authority; read them for how something works.

## Global Invariants

1. Business behavior lives in `crates/<feature>`; transports and providers in
   `crates/infra-<provider>`; concrete wiring and process lifecycle in the
   composition root `crates/service`. The crate graph is the dependency rule:
   features' HTTP modules may use `infra-http`'s inbound contract surfaces,
   no feature depends on a provider crate, and no crate a feature depends on
   may depend on that feature; the compiler refuses the reverse edge.
2. Generated output is derived. `api/openapi/service.yaml` is generated from
   the handlers' `#[utoipa::path]` annotations and schema derives, committed,
   and byte-compared by a test; the annotations are where a change is made.
3. One fact has one canonical writer. A projection, transport, cache, or
   generated view does not become authority because it is easier to inspect.
4. Optional profiles remain inert unless selected and fully wired; a partial
   capability is not a supported architecture. A crate, module, or directory
   exists only with its first real artifact.
5. Trust, ingress and egress, dependency criticality, failure and recovery,
   and rollout choices are explicit at the boundary that enforces them: the
   hardened chain, the bounded server, the config validation, the readiness
   refresher, the staged teardown.
6. A task-local design may retain, replace, or remove current architecture,
   but it names the changed owner and the proving surface.

## Source Of Truth

| Source | Derived or consuming surfaces |
| --- | --- |
| `#[utoipa::path]` annotations, `ToSchema`/`IntoResponses` derives, and the router merge in `crates/service/src/api.rs` | `api/openapi/service.yaml` (generated, committed, drift-tested), the served axum `Router` |
| `crates/config/src/<section>.rs` (type, defaults, validation) | The immutable `Config` snapshot bootstrap and the adapters read |
| `env/config/*.toml`, `APP__SECTION__KEY`, `--config`, `--config-overlay`, `--secrets-dir` | Inputs whose precedence and secret rules `crates/config/src/load.rs` implements |
| `crates/health` | The readiness verdict `/health/ready` serves and the drain flag teardown flips |
| `crates/service-failure` | The closed catalog of failure codes; each transport owns its wire projection |
<!-- template:begin grpc:docs-architecture-grpc-source -->
| `api/proto/`, `tools/grpc-codegen` | Committed prost/tonic contracts under `crates/grpc-contracts`, checked against the PR base |
| `crates/infra-grpc` | Tonic routes on the shared listener, clients, health projection and server TLS |
<!-- template:end grpc:docs-architecture-grpc-source -->
| `crates/service/src/bootstrap` | Startup order, the shutdown plan, exit codes |
| `crates/<feature>` (none yet) | Behavior consumed by transports and future binaries |
| `tools/versions.env`, `deny.toml`, `.gitleaks.toml`, `build/docker/Dockerfile` | Tool pins and gate policy consumed by `make` and CI ([CI/CD Production Readiness](ci-cd-production-ready.md)) |

<!-- template:begin postgres:docs-architecture-schema -->
`migrations/*.sql` owns the PostgreSQL schema; `crates/migrate` embeds and applies
it, and access code adapts to it.
<!-- template:end postgres:docs-architecture-schema -->

Concrete adapter wiring belongs in the composition root. Generated outputs
are never edited as the source of truth.

## Domain Vocabulary

Keep only accepted cross-task terms whose interpretation changes behavior,
violation outcome, authority, proof, or handoff. Task-local or unsettled
terms stay in their owning specification.

| Term | Means here | Does not mean | Authority source | Semantic owner | Decision affected |
| --- | --- | --- | --- | --- | --- |

The scaffold defines no service-specific terms. Add rows
only for stable domain decisions.

## Other Documents

| Changed pressure | Load |
| --- | --- |
| Executable dependency direction between crates | [`quality/architecture.json`](../quality/architecture.json), checked by `make architecture-check` |
| Proof or validation must be selected | [Validation Routing](validation-routing.md) |
| Build, test, or generator commands | `make help` and [`make/template.mk`](../make/template.mk) |
| Choosing a library or a recorded library decision | [Backend Library Selection](backend-library-selection.md) and the records under `docs/decisions/` |
| Delivery gates, the image, publication, and production readiness | [CI/CD Production Readiness](ci-cd-production-ready.md) |
| Deployment policy for a derived service | [Railway Deployment Profile](railway-deployment-profile.md) |
| What a service must decide before production | [Production Contract](production-contract.md) |
| Alerts and their runbooks | [Alerting](alerting.md) |
