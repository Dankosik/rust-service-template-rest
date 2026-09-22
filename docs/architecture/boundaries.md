# Component Boundaries

Load when crate, dependency, source-of-truth, or composition ownership can
change. Current code and crate documentation remain the final factual
authority; the crate graph in `Cargo.toml` is what the compiler enforces.

| Crate (path) | Owns | Does not own |
| --- | --- | --- |
| Service package (`crates/service/Cargo.toml`) | The main binary named by that manifest: `main` maps the bootstrap result to an exit code; `bootstrap` composes configuration, telemetry, readiness, the route tree, the two listeners, background tasks, signals, and the staged teardown; `api` merges every `OpenApiRouter` into the one contract whose halves are the served router and the document; the `openapi` binary renders it; the process tests drive the built binary. | Business behavior, request handling beyond composition, provider details. |
| `service-config` (`crates/config`) | One validated immutable snapshot: section types with defaults and validation in `<section>.rs`, loader precedence, the `APP__` name pre-scan, the secret-in-file refusal, `SecretString` fields, human-form durations and sizes, build metadata (`app.version`, `app.commit`). | Feature behavior, dependency wiring, request handling, telemetry construction. |
| `health` (`crates/health`) | The readiness refresher over `tokio::sync::watch`: probe trait, failure threshold, staleness guard, drain flag, O(1) snapshot reads. | Probe implementations, HTTP handlers, the schedule (bootstrap owns the policy values). |
| `infra-http` (`crates/infra-http`) | The hardened middleware chain, the bounded accept loop (`Server`), the probe handlers with their `#[utoipa::path]` contract, the RFC 9457 `Problem` type and closed code catalog, request-id admission, the route-template access log. | Business rules, configuration loading, feature routes (they merge in `service::api`). |
| `infra-telemetry` (`crates/infra-telemetry`) | Subscriber installation (`json`/`text`), the tracer provider with the OTLP endpoint resolution and ambient-credential refusal, the Prometheus recorder with process and Tokio runtime metrics, the diagnostics router. | Feature semantics, startup logging content, request routing, which fields a handler emits. |
| `integration-tests` (`test/`) | Executable utility recipes and any selected profile proof. | Anything a binary runs; the service's process tests stay in `crates/service/tests/`. |
| `crates/<feature>` (none yet) | Use cases, business types, invariants, domain errors, and the feature's `OpenApiRouter` with its handlers. | Transport policy, provider drivers, runtime configuration, process lifecycle. |
| `crates/infra-<provider>` (further adapters) | One transport or provider adapter: admission, budgets, mapping to feature-owned types. | Business rules, config precedence, other adapters' policy. |
| `api/openapi/service.yaml` | The committed, reviewed, lint-checked, compatibility-judged form of the contract. | Runtime logic; it is generated, never edited. |
| `env/config/local.toml` | The local baseline for `make run`. | Deployment values (`APP__*` environment). |

<!-- template:begin postgres:docs-boundaries-postgres-owners -->
## PostgreSQL owners

The selected PostgreSQL profile adds these owners:

| Crate (path) | Owns | Does not own |
| --- | --- | --- |
| `infra-postgres` (`crates/infra-postgres`) | The PostgreSQL adapter: DSN admission, the pool with the template's session budgets, the readiness probe, pool gauges, the transaction seam with its commit-outcome policy ([Persistence](persistence.md)). | Business rules, when the pool opens or closes, schema, configuration precedence. |
| `migrate` (`crates/migrate`) | The embedded migration set, the runner over one connection with lock, budgets, deadline, stages, and terminal record; the `migrate` binary. | Schema content (`migrations/`), the pool, readiness. |

Its database-backed tests use the `integration-tests` package's opt-in
`integration` feature and owned migration fixtures.

<!-- template:end postgres:docs-boundaries-postgres-owners -->

## Dependency Direction

```text
main binary (crates/service, composition root)
  -> service-config
  -> health
  -> infra-http      -> health, axum, tower, tower-http, hyper-util, utoipa, utoipa-axum
  -> infra-telemetry -> opentelemetry*, tracing*, metrics*
  -> crates/<feature> (future; depends on no infra-* crate)

integration-tests (test/)
  -> utility and transport recipes, health
```

<!-- template:begin postgres:docs-boundaries-postgres-edges -->
The PostgreSQL profile also adds `infra-postgres -> health, sqlx, url, metrics`,
bootstrap's dependency on `infra-postgres`, and the `migrate` binary/library
depending on `infra-postgres`, `service-config`, `infra-telemetry` and `sqlx`.
Its integration tests also depend on the provider and migrator.
<!-- template:end postgres:docs-boundaries-postgres-edges -->

Feature crates never depend on a transport or provider crate; bootstrap may
know every adapter because it is the composition root. Shared contracts start
beside their real consumer and move only for observed reuse. `health` is the
one leaf two crates share (`infra-http` reads the verdict, `service` drives
the refresher), which is why it is its own crate rather than a module of
either.

## Decisions Recorded Here

Ownership choices made in stages 1 to 3 and 8 that a later change should not
silently reopen:

- **One crate per ownership boundary** rather than modules of one binary:
  the graph enforces dependency direction at compile time, which the Go
  template needs `depguard` for.
- **`service-config` is named for the crate, `config` for the directory**: the
  package name avoids a collision with the `config` crate it wraps; a
  changed path maps to the package through its manifest, not its directory.
- **`infra-http` owns the probe handlers and their contract**, because the
  probes are platform behavior every derived service keeps; feature
  operations merge beside them in `service::api::contract()`.
- **The `Problem` type is template-owned** (about sixty lines) with `code`,
  `request_id`, and `invalid_params` first-class; `problem_details` was the
  acceptable crate alternative and may replace it if the catalog outgrows the
  type.
- **The readiness refresher is template-owned** (`health`, about 150 lines):
  no maintained crate publishes a cached verdict with a failure threshold,
  staleness guard, and drain flag (`health` on crates.io is unmaintained since
  2022; `axum-health` probes per request).
- **The accept loop is template-owned** (about eighty lines over
  `hyper_util::server::conn::auto`): `axum::serve` sets no timer and exposes
  no connection limits, so hyper's header timeout is silently disabled there
  (axum #2741) and the `auto` builder starts no timer until the first byte
  (hyper #3756); the loop adds the permit, the peek, and the timer.
<!-- template:begin postgres:docs-boundaries-migrator-decision -->
- **`migrate` is a library plus a binary in one crate** so the runner is
  testable against fixture migrators while the binary embeds the real set;
  it depends on `infra-postgres` for admission and budget rendering, never
  the reverse. Persistence decisions: [Persistence](persistence.md#decisions-recorded-here).
<!-- template:end postgres:docs-boundaries-migrator-decision -->
- **`test/` is the package `integration-tests`**: a package named `test`
  collides with the built-in test crate.
