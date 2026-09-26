# Component Boundaries

Load when crate, dependency, source-of-truth, or composition ownership can
change. Current code and crate documentation remain the final factual
authority; the crate graph in `Cargo.toml` is what the compiler enforces.

| Crate (path) | Owns | Does not own |
| --- | --- | --- |
| Service package (`crates/service/Cargo.toml`) | The main binary named by that manifest: `main` maps the bootstrap result to an exit code; `bootstrap` composes configuration, telemetry, readiness, the route tree, the two listeners, background tasks, signals, and the staged teardown; `api` merges every `OpenApiRouter` into the one contract and finalizes its served router; the `openapi` binary renders its document; the process tests drive the built binary. | Business behavior, request handling beyond composition, provider details. |
| `service-config` (`crates/config`) | One validated immutable snapshot: section types with defaults and validation in `<section>.rs`, loader precedence, the `APP__` name pre-scan, the secret-in-file refusal, `SecretString` fields, human-form durations and sizes, build metadata (`app.version`, `app.commit`). | Feature behavior, dependency wiring, request handling, telemetry construction. |
| `health` (`crates/health`) | The readiness refresher over `tokio::sync::watch`: probe trait, failure threshold, staleness guard, drain flag, O(1) snapshot reads. | Probe implementations, HTTP handlers, the schedule (bootstrap owns the policy values). |
| `infra-http` (`crates/infra-http`) | The hardened middleware chain, the bounded accept loop (`Server`), the probe handlers with their `#[utoipa::path]` contract, the RFC 9457 `Problem` type and closed code catalog, request-id admission, the route-template access log. | Business rules, configuration loading, feature routes (they merge in `service::api`). |
| `infra-telemetry` (`crates/infra-telemetry`) | Subscriber installation (`json`/`text`), the tracer provider with the OTLP endpoint resolution and ambient-credential refusal, the Prometheus recorder with process and Tokio runtime metrics, the diagnostics router. | Feature semantics, startup logging content, request routing, which fields a handler emits. |
<!-- template:begin authn:docs-boundaries-authn-owner -->
| `infra-bearerauthn` (`crates/infra-bearerauthn`) | Bearer-envelope parsing, sealed verified identity and immutable typed claims access, canonical provider URL admission, and the selected OIDC JWT or introspection verifier with its trusted provider transport. | Authorization policy, configuration loading, route assembly, readiness, or application-visible raw tokens or mutable claim evidence. |
<!-- template:end authn:docs-boundaries-authn-owner -->
<!-- template:begin outbound-http:docs-boundaries-outbound-owner -->
| `infra-outbound-http` (`crates/infra-outbound-http`) | Fixed trusted-origin HTTPS exchanges over standard `http::Request<Bytes>`/`Response<Bytes>`, finite limits, component target composition, operation lifetime, and private attempt observation ([guide](../outbound-http.md)). | Provider credentials, parsing, retries, configuration, readiness, bootstrap, or task tracking. |
<!-- template:end outbound-http:docs-boundaries-outbound-owner -->
<!-- template:begin outbound-auth:docs-boundaries-outbound-auth-owner -->
| `infra-oauth2-client-credentials` (`crates/infra-oauth2-client-credentials`) | Private, per-immutable-tuple OAuth2 client-credentials acquisition and reuse, sanitized failures, and one authenticated bounded resource client ([guide](../outbound-machine-authentication.md)). | Named configuration loading, provider registration, readiness, bootstrap, business mapping, a public token source, or gRPC composition. |
<!-- template:end outbound-auth:docs-boundaries-outbound-auth-owner -->
<!-- template:begin http-idempotency:docs-boundaries-http-idempotency-owner -->
| `infra-idempotency-store` (`crates/infra-idempotency-store`) | PostgreSQL idempotency arbitration and durable records ([guide](../http-idempotency.md)). | Migration-history admission, transaction lifecycle/connection ownership, HTTP types/Problems, business rules, readiness registration, or request routing. |
<!-- template:end http-idempotency:docs-boundaries-http-idempotency-owner -->
<!-- template:begin jobs:docs-boundaries-jobs-owners -->
| `infra-jobs` (`crates/infra-jobs`) | The job table's statements, enqueue, the job-kind and handler contracts (`JobKind`, `Handler`, `Kinds`), and the engine ([guide](../background-jobs.md)). | Concrete kinds or handlers (they live in adapter crates), configuration, process lifecycle, or business rules. |
| `jobs-worker` (`crates/jobs-worker`) | The worker's composition root and binary. | Engine mechanics, feature behavior. |
<!-- template:end jobs:docs-boundaries-jobs-owners -->

| `integration-tests` (`test/`) | Executable utility recipes and any selected profile proof. | Anything a binary runs; the service's process tests stay in `crates/service/tests/`. |
| `crates/<feature>` (none yet) | Use cases, business types, invariants, domain errors, and the feature's `OpenApiRouter` registrations with its handlers. | Transport policy, provider drivers, runtime configuration, process lifecycle. |
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
<!-- template:begin jobs:docs-boundaries-jobs-tests -->
`integration-tests` also owns the jobs suites and one test-only binary,
`jobs-worker-fixture`, which runs the worker composition with a test-only
kind for the process proof and which no image ships. The owners table's
"Anything a binary runs" concerns shipped binaries; the shipped worker's
process tests stay in `crates/jobs-worker/tests/`.

<!-- template:end jobs:docs-boundaries-jobs-tests -->

## Dependency Direction

```text
main binary (crates/service, composition root)
  -> service-config
  -> health
  -> infra-http      -> health, axum, tower, tower-http, hyper-util, utoipa, utoipa-axum
  -> infra-telemetry -> opentelemetry*, tracing*, metrics*
  -> crates/<feature> (future; depends on infra-http's inbound contract
     surfaces, never a provider crate)

<!-- template:begin authn:docs-boundaries-authn-edges -->
  -> infra-bearerauthn
infra-http -> infra-bearerauthn
<!-- template:end authn:docs-boundaries-authn-edges -->
<!-- template:begin outbound-http:docs-boundaries-outbound-edges -->
infra-outbound-http -> reqwest, http, bytes, url, tokio, metrics, tracing
<!-- template:end outbound-http:docs-boundaries-outbound-edges -->
<!-- template:begin outbound-auth:docs-boundaries-outbound-auth-edges -->
infra-oauth2-client-credentials -> infra-outbound-http, oauth2, moka, http, bytes, tokio, secrecy
<!-- template:end outbound-auth:docs-boundaries-outbound-auth-edges -->
<!-- template:begin http-idempotency:docs-boundaries-http-idempotency-edges -->
main binary -> infra-idempotency-store
infra-http -> infra-idempotency-store, infra-postgres (the Tx re-export), sha2, sfv
infra-idempotency-store -> infra-postgres, sqlx, tokio, tokio-util, tracing, thiserror
<!-- template:end http-idempotency:docs-boundaries-http-idempotency-edges -->
<!-- template:begin jobs:docs-boundaries-jobs-edges -->
jobs-worker (composition root) -> service-config, health, infra-http,
                                  infra-telemetry, infra-postgres, infra-jobs
infra-jobs -> infra-postgres, sqlx, serde, serde_json, uuid, opentelemetry,
              tracing-opentelemetry, metrics, humantime, tokio, tokio-util,
              tracing, thiserror
a feature's infra-<provider> adapter -> infra-jobs
<!-- template:end jobs:docs-boundaries-jobs-edges -->


integration-tests (test/)
  -> utility and transport recipes, health
```

<!-- template:begin postgres:docs-boundaries-postgres-edges -->
The PostgreSQL profile also adds `infra-postgres -> health, sqlx, url, metrics`,
bootstrap's dependency on `infra-postgres`, and the `migrate` binary/library
depending on `infra-postgres`, `service-config`, `infra-telemetry` and `sqlx`.
Its integration tests also depend on the provider and migrator.
<!-- template:end postgres:docs-boundaries-postgres-edges -->

A feature's HTTP module may use `infra-http`'s inbound contract surfaces; no
feature depends on a provider crate, and no crate a feature depends on may
depend on it, so the compiler refuses the reverse edge. Bootstrap may know
every adapter because it is the composition root. Shared contracts start
beside their real consumer and move only for observed reuse. `health` is a
leaf two crates share (`infra-http` reads the verdict, `service` drives
the refresher), which is why it is its own crate rather than a module of
either.

<!-- template:begin authn:docs-boundaries-authn-composition -->
Authentication is a shared inbound transport contract, not a feature adapter:
`service` owns verifier preparation and lifecycle; `infra-http` finalizes the
assembled `OpenApiRouter` into one policy layer and maps Problems. Feature
handlers consume only the sealed `VerifiedPrincipal`; they do not opt individual
routes into authentication.
<!-- template:end authn:docs-boundaries-authn-composition -->
<!-- template:begin http-idempotency:docs-boundaries-http-idempotency-composition -->
HTTP idempotency is a shared inbound transport contract, not a feature
adapter: `infra-http` composes it through fallible `Composer::route` and owns
key handling, declaration, response provenance, and Problem mapping. The
service assembly calls `finish` to activate the successfully composed routes.
`infra-idempotency-store`
owns PostgreSQL arbitration and records. `infra-postgres` owns the opaque
`Tx`; `infra_http::idempotency` re-exports it only as an inbound contract, so
`infra-http` depends on `infra-postgres` only while this profile is retained.
Feature handlers consume `Idempotency` and that `Tx` through the supported
composed route, while provider adapters alone use the connection.
<!-- template:end http-idempotency:docs-boundaries-http-idempotency-composition -->
<!-- template:begin outbound-auth:docs-boundaries-outbound-auth-composition -->
OAuth2 machine authentication is a provider-infrastructure boundary: a concrete
adapter translates named config into private credentials, binds them to its
bounded resource client, and maps its own business errors. The service has no
OAuth registry or readiness wiring, and features do not receive tokens.
<!-- template:end outbound-auth:docs-boundaries-outbound-auth-composition -->
<!-- template:begin jobs:docs-boundaries-jobs-composition -->
Jobs are a provider seam, not a transport contract: an adapter enqueues on
the connection it already holds; kinds and handlers live in adapter crates
and call feature use cases; features never depend on `infra-jobs`; the
service composes nothing for jobs; the worker composes its own process.
<!-- template:end jobs:docs-boundaries-jobs-composition -->

## Decisions Recorded Here

<!-- template:begin webhooks-common:docs-boundaries-webhooks-provider -->
## Webhook provider boundary

`infra-webhooks` owns Standard Webhooks framing/verification, durable outbound
delivery and inbound receipt/processing adapters. `infra-http` owns only inbound
transport; `service` and `jobs-worker` remain composition roots. Feature code
uses its provider adapter and never depends on jobs, PostgreSQL, transport, or
webhook protocol types directly. This avoids both a reverse root edge and a
second queue or receipt-store owner.
<!-- template:end webhooks-common:docs-boundaries-webhooks-provider -->

<!-- template:begin webhooks:docs-boundaries-webhooks-outbound -->
The outbound module owns prepared endpoint metadata, the
`webhooks.deliver` handler, and a fixed startup map of endpoint clients and key
rings. Queued work stores endpoint identity and body; each attempt resolves the
current snapshot. It may use
jobs, PostgreSQL, the existing bounded outbound HTTP client, and protocol glue;
it does not own business acceptance, endpoint management, DNS policy, or retry
scheduling.
<!-- template:end webhooks:docs-boundaries-webhooks-outbound -->

<!-- template:begin inbound-webhooks:docs-boundaries-webhooks-inbound -->
The inbound module owns raw-byte verification, receipt arbitration, and the
consumer registry/processor. It may use protocol, jobs, PostgreSQL, and SQLx;
it does not own router middleware, endpoint configuration precedence, or a
business event schema. `infra-http::webhooks` owns route annotation and problem
mapping, not receipt SQL or signature implementation.

`webhook-consumers` is the shared adopter composition crate retained only with
inbound webhooks. Its `consumers()` constructor is the one registration edit
point used by both service and worker; both roots reject unbound configured
endpoints before serving or claiming. It initially depends only on
`infra-webhooks`; adopter adapters keep business behavior in feature owners.
The worker passes the same registry into the processor. Neither root depends
on the other, and the provider does not own adopter registrations.
<!-- template:end inbound-webhooks:docs-boundaries-webhooks-inbound -->

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
<!-- template:begin jobs:docs-boundaries-jobs-decision -->
- **`jobs-worker` is a library plus a binary in its own crate**: the
  initializer requires the service package's binaries to be exactly the main
  binary and `openapi`, and the process tests need the library entry with a
  test-only kind.
<!-- template:end jobs:docs-boundaries-jobs-decision -->
- **`test/` is the package `integration-tests`**: a package named `test`
  collides with the built-in test crate.
