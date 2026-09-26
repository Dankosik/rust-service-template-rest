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
(`test/tests/postgres.rs`), including pending-BEGIN cancellation and read-only
embedded-history admission/refusal. Extra arguments reach `cargo test`:
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
added version older than the newest the base already has. Its only
pre-adoption exception recognizes the reviewed replacement of the two
idempotency/jobs create blobs and deletion of their two superseded simplify
blobs as one exact four-file transition; it rejects partial or other rewrites.
The source rules (positive version, forward-only, one transaction,
`snake_case`) are the `migrate` crate's tests over the embedded set. Neither
needs Docker.

The runtime rehearsal, when the image's migration path or readiness with the
pool open is the claim:

```bash
ALLOW_HEAVY=1 make migration-validate RUNTIME_EXPECTED_COMMIT="$(git rev-parse HEAD)"
```

For a nonempty embedded set, it first checks that service startup refuses
missing history within 30 seconds, before the migrator creates bookkeeping.
It then runs `/migrate` from the image against the fresh compose database under
the hardened flags, requires a `migration_run` record with outcome `success` or
`no_change`, requires `no_change` from a second run, and runs the lifecycle
check with `APP__POSTGRES__ENABLED=true`, asserting the `postgres_pool_opened`
record beside the usual readiness and `SIGTERM` evidence. The Make target uses
the local runtime-image default from `make/service.mk`.

Missing Docker is not a pass for a required scenario. If the scenario is
optional, disclose the gap and stop without building or repairing a test
environment; it does not block local completion.
<!-- template:end postgres:docs-postgres-validation -->

<!-- template:begin http-idempotency:docs-postgres-validation-http-idempotency -->
With the HTTP idempotency profile retained, `ALLOW_HEAVY=1 make test-integration-db`
also runs the idempotency suite in `test/tests/http_idempotency/`:
arbitration, equal replay and mismatch, expiry, writer refusal, rollback,
same-key retry after uncertainty, 25P02 classification, byte-safe replay,
caller metadata, long caller fields, and activation proof
against the store boundary, plus the mounted router proof where the
introspection engine is retained.
`bash scripts/ci/test-integration-db.sh --test http_idempotency` runs that
target alone (the script forwards its arguments to `cargo test`). The suite
uses the existing transaction/store error seam for uncertain commits; it does
not require a lost-COMMIT-ack proxy or post-commit readback. It establishes
database observations only when a usable Docker daemon is available. Its
cases include cancellation and outer-504 uncertainty returning to same-key
arbitration, and the one-outcome metric semantics. This is proof of local database behavior only;
it does not authorize a deployment.
<!-- template:end http-idempotency:docs-postgres-validation-http-idempotency -->
<!-- template:begin jobs:docs-postgres-validation-jobs -->

With the jobs pack retained, `ALLOW_HEAVY=1 make test-integration-db` also
runs the jobs suite in `test/tests/jobs/`: `enqueue.rs` (atomicity through
`in_tx`: a committed transaction enqueues, a rolled-back one does not;
validation and database failure classes; every uniqueness row, including
`40001` under `REPEATABLE READ`), `execution.rs` (claiming and exclusivity
with two engines racing, retry scheduling, both terminal reasons, timeout,
recovery of a lost worker's job within its bound, rejection of a superseded
attempt's outcome, unknown kinds, retention; expiry cases are staged by
setting database times), `process.rs` (the test-only `jobs-worker-fixture`
binary, built only with the `integration` feature and shipped by no image:
refusals with PostgreSQL disabled and with migration history missing, ready
then a committed job then exit `0` on `SIGTERM` with the `/metrics` lines,
and an attempt that outlives a short drain exiting `3` with its job claimable
again), and, where the HTTP idempotency pack is also retained,
`http_idempotency.rs` (the joint proof through the idempotency store).
`bash scripts/ci/test-integration-db.sh --test jobs` runs that target alone.
In the source template's initializer matrix, runtime graphs 17-26 run this
suite once each (23-26 with the joint module) and need a usable Docker
daemon.
<!-- template:end jobs:docs-postgres-validation-jobs -->
