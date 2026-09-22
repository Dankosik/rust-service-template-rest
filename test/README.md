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
