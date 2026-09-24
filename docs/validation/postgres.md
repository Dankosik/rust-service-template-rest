# PostgreSQL Validation

Read [local persistence availability](../architecture/persistence.md) first.
When `template.lock` selects `database: none`, database validation commands are
absent; this record does not authorize reconstructing the removed profile.
A retained profile uses the real-server proof below when its claim requires it.

<!-- template:begin postgres:docs-postgres-validation -->
Use for an explicitly required database observation or a bounded diagnostic.
Changing a PostgreSQL file does not add a local integration gate; ordinary
completion for `crates/infra-postgres`, `crates/migrate`, and `test/` still
follows [Rust Validation](rust.md), whose unit tests need no Docker.

A claim of observed transaction, locking, commit-outcome, readiness,
migration, or adapter-integration behavior needs a real PostgreSQL:

```bash
ALLOW_HEAVY=1 make test-integration-db
```

The script brings `env/docker-compose.yml` up under a throwaway project on an
ephemeral port, clears the libpq variables the DSN policy refuses, exports
`DATABASE_URL` in the admitted URL form, runs
`cargo test --locked -p integration-tests --features integration`, and tears
the project down. Every test gets its own `_sqlx_test_*` database from
`#[sqlx::test]`; the template's pool, probe, transaction seam, and runner are
exercised through the admitted `Dsn` exactly as the binaries use them
(`test/tests/postgres.rs`). Extra arguments reach `cargo test`:
`bash scripts/ci/test-integration-db.sh migrations_apply -- --nocapture`.
Without Docker the target refuses with exit 2; `REQUIRE_DOCKER=1` (what CI
sets) turns that into a failure, because a skipped required test is not a
pass.

Migration source shape and the static append-only rule:

```bash
make migration-check            # worktree against HEAD, untracked files included
make migration-check BASE_REF=origin/main
```

The history check refuses a modified, deleted, or renamed migration and an
added version older than the newest the base already has; the source rules
(positive version, forward-only, one transaction, `snake_case`) are the
`migrate` crate's tests over the embedded set. Neither needs Docker.

The runtime rehearsal, when the image's migration path or readiness with the
pool open is the claim:

```bash
ALLOW_HEAVY=1 make migration-validate RUNTIME_EXPECTED_COMMIT="$(git rev-parse HEAD)"
```

It runs `/migrate` from the image against a fresh compose database under the
hardened flags, requires a `migration_run` record with `outcome`
`success` or `no_change`, requires `no_change` from a second run, and then
runs the lifecycle check with `APP__POSTGRES__ENABLED=true`, asserting the
`postgres_pool_opened` record beside the usual readiness and `SIGTERM`
evidence. The Make target uses the local runtime-image default from `make/service.mk`.

Missing Docker is not a pass for a required scenario. If the scenario is
optional, disclose the gap and stop without building or repairing a test
environment; it does not block local completion.
<!-- template:end postgres:docs-postgres-validation -->

<!-- template:begin http-idempotency:docs-postgres-validation-http-idempotency -->
With the HTTP idempotency profile retained, `ALLOW_HEAVY=1 make test-integration-db`
also runs the idempotency suite in `test/tests/http_idempotency/`:
arbitration, replay, expiry, writer-refusal, unknown-commit, abandonment,
and activation proof against the store boundary (P1-P8), plus the mounted
router proof (P9) where the introspection engine is retained.
`bash scripts/ci/test-integration-db.sh --test http_idempotency` runs that
target alone (the script forwards its arguments to `cargo test`). The
unknown-commit case (P6) uses a one-shot test proxy that acts on one
`COMMIT`: it either forwards it and drops the acknowledgement, or closes
both sockets before forwarding it, so the request sees a lost
acknowledgement around a commit that did or did not happen; the readback's
fresh connection passes through untouched. In the source template's
initializer matrix, runtime graphs 13-16 run this suite once each, after
their build and test, and need a usable Docker daemon.
<!-- template:end http-idempotency:docs-postgres-validation-http-idempotency -->
