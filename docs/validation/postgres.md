# PostgreSQL Validation

When `template.lock` selects `database: none`, database validation commands are
absent; this record does not authorize reconstructing the removed profile.

<!-- template:begin postgres:docs-postgres-validation -->
Use only for an explicitly required database observation or a bounded
diagnostic. Ordinary local completion for `crates/infra-postgres`,
`crates/migrate`, and `test/` follows [AGENTS.md](../../AGENTS.md#validation-budget),
whose unit tests need no Docker.

## Real-server integration and metadata checks

| Claim | Command | Notes |
| --- | --- | --- |
| Real transactions, locking, `CommitUnknown`, pool return/saturation, readiness, or password rotation | `ALLOW_HEAVY=1 make test-integration-db` | Starts ephemeral Compose PostgreSQL + transaction-mode PgBouncer and runs `integration-tests --features integration` (`bash scripts/ci/test-integration-db.sh --test postgres -- --no-capture` scopes to one target; `INTEGRATION_COMPOSE_MANAGED=1` reuses caller-managed URLs). Refuses with exit 2 if Docker is absent (`REQUIRE_DOCKER=1` fails in CI). |
| Checked `sqlx::query!` metadata (`.sqlx/`) | `make sqlx-prepare` / `ALLOW_HEAVY=1 make sqlx-check` | Applies `migrations/` on ephemeral Compose PostgreSQL with pinned `sqlx-cli` (must match `Cargo.toml`'s `sqlx` pin) and verifies `.sqlx/` against live schema. |
| Static append-only migration history and DDL safety | `make migration-check` (`BASE_REF=origin/main`) | Refuses modified/deleted/renamed/out-of-order migrations without Docker, and runs Squawk (`npx`) on added `.sql` files (`-- no-transaction` linted outside a transaction). |
| Runtime image `/migrate` and startup history admission | `ALLOW_HEAVY=1 make migration-validate RUNTIME_EXPECTED_COMMIT="$(git rev-parse HEAD)"` | Verifies service refusal on missing history, idempotent `/migrate` runs, and `postgres_pool_opened` readiness + `SIGTERM` lifecycle. |

When modifying the vendored SQLx patch, also verify
[`vendor/sqlx-core/PATCHES.md`](../../vendor/sqlx-core/PATCHES.md) and locked
profile projections. Missing Docker on an optional check is a disclosed gap,
not a blocker to local completion.

## Workload-sizing discipline

Local integration suites prove recovery and finality invariants, not production
capacity. For sizing experiments, budget total connections across peak service
and worker replicas (including rollout overlap), dedicated `LISTEN` sessions,
migrator/admin reserves, and PgBouncer front/back pools before changing a single
knob, and revert if acquisition timeouts, lock waits, or server pressure worsen.
<!-- template:end postgres:docs-postgres-validation -->

<!-- template:begin postgres:docs-postgres-validation-http-consumers -->
`test/tests/postgres/` includes two-instance HTTP consumer isolation and
shared-PostgreSQL recovery checks; they prove local adapter behavior once
executed by `test-integration-db.sh`, not fleet capacity.
<!-- template:end postgres:docs-postgres-validation-http-consumers -->

<!-- template:begin postgres-grpc-consumers:docs-postgres-validation-consumers -->
When PostgreSQL and gRPC are retained (`AUTHN=none` or `oidc-introspection`),
`bash scripts/ci/test-integration-db.sh --test postgres` also covers the
two-instance gRPC Echo and health `Watch` recovery path in
`test/tests/postgres/operational_recovery.rs`.
<!-- template:end postgres-grpc-consumers:docs-postgres-validation-consumers -->

<!-- template:begin http-idempotency:docs-postgres-validation-http-idempotency -->
With HTTP idempotency retained, `ALLOW_HEAVY=1 make test-integration-db` (or
`bash scripts/ci/test-integration-db.sh --test http_idempotency`) exercises
replay, payload mismatch, and wire-proxy `COMMIT` uncertainty recovery.
<!-- template:end http-idempotency:docs-postgres-validation-http-idempotency -->
<!-- template:begin jobs:docs-postgres-validation-jobs -->

With background jobs retained, `ALLOW_HEAVY=1 make test-integration-db` (or
`bash scripts/ci/test-integration-db.sh --test jobs`) exercises transactional
`in_tx` enqueue, fenced claiming/reclaim, and `jobs-worker-fixture` drain/exit
behavior.
<!-- template:end jobs:docs-postgres-validation-jobs -->
