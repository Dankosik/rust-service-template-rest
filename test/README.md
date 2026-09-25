# Integration tests

This service retains this crate for utility-recipe tests. PostgreSQL-specific
proof is available only when the local PostgreSQL profile is retained.

<!-- template:begin postgres:test-readme-postgres -->
Workspace crate `integration-tests`: database-backed proof for the PostgreSQL
profile. `make test` compiles it with the `integration` feature off, so
nothing here runs without Docker; `ALLOW_HEAVY=1 make test-integration-db`
brings a PostgreSQL up from `env/docker-compose.yml`, exports
`DATABASE_URL`, and runs `tests/postgres.rs` with the feature on.

- `src/lib.rs`: helpers that turn the `#[sqlx::test]` per-test database into
  an admitted `Dsn`.
- `tests/postgres.rs`: pool session defaults, probe verdicts, the transaction
  seam and its commit-outcome policy, and the migration runner's stages.
- `fixtures/migrations/<scenario>/`: migration sets the runner tests apply;
  not part of the service schema, which lives in `migrations/`.

Selection and the claims each command supports:
[PostgreSQL Validation](../docs/validation/postgres.md). Process and
container proof for the built image stays in `crates/service/tests/` and
`scripts/ci/runtime-image-check.sh`.
<!-- template:end postgres:test-readme-postgres -->

<!-- template:begin http-idempotency:test-readme-http-idempotency -->
With the HTTP idempotency profile retained, the same command also runs
`tests/http_idempotency/`. Its `main.rs` proves arbitration, replay, rollback,
expiry and cleanup, writer refusal, unknown commits, abandonment, and the
startup check at the record store, with two independent pools standing for
two replicas (P1-P8). `commit_proxy.rs` is the one-shot wire proxy that lets a
commit happen, or not, and loses its acknowledgement. The mounted HTTP proof
(P9) runs only where the introspection engine is retained: in the source
template and in `AUTHN=oidc-introspection` outputs.
<!-- template:end http-idempotency:test-readme-http-idempotency -->
<!-- template:begin jobs:test-readme-jobs -->

With the jobs pack retained, the same command also runs `tests/jobs/`: the
engine suite (enqueue, uniqueness, claiming, retries, recovery, fencing,
retention; `main.rs` with `enqueue.rs` and `execution.rs`), the process suite
on the test-only `jobs-worker-fixture` binary (`process.rs`, with the `Probe`
kind in `src/jobs.rs` and the binary in `src/bin/`), and the joint HTTP
idempotency proof (`http_idempotency.rs`) where both packs are retained. The
shipped binary's refusal test is in `crates/jobs-worker/tests/`.
<!-- template:end jobs:test-readme-jobs -->
