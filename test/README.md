# Integration tests

This service retains this crate for utility-recipe tests. PostgreSQL-specific
proof is available only when the local PostgreSQL profile is retained.

<!-- template:begin postgres:test-readme-postgres -->
Workspace crate `integration-tests`: database-backed proof for the PostgreSQL
profile. `make test` compiles it with the `integration` feature off, so
nothing here runs without Docker; `ALLOW_HEAVY=1 make test-integration-db`
brings a PostgreSQL and a PgBouncer in front of it up from
`env/docker-compose.yml`, exports `DATABASE_URL` and
`PGBOUNCER_DATABASE_URL`, and runs `tests/postgres.rs` with the feature on.

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
`tests/http_idempotency/`. Its store and mounted-router cases prove real
PostgreSQL arbitration, equal replay/mismatch, rollback and expiry, same-key
retry after uncertainty, 25P02 mapping, byte-preserving seven-header replay,
and caller metadata. They use the ordinary provider
error seam; this suite uses no commit proxy (the jobs suite keeps its own in
`tests/support/commit_proxy.rs`), forced lost-COMMIT acknowledgement, readback,
or replacement fault framework. The mounted HTTP proof runs where the
introspection engine is retained.
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

<!-- template:begin jobs-reference:test-readme-reading-reference -->
## Reading-counter recovery reference

`src/reading_counter.rs` is a service-owned example with a local CLI in
`src/bin/reading_counter_fixture.rs`. It is retained with jobs, PostgreSQL
outbox, JetStream and durable outbound webhooks. The shipped service exposes no
reading endpoint and embeds none of its fixture schema.

One bounded immutable operation accepts a request and three intents in the
same transaction: local reading job, accepted-reading event and webhook. Its
stable `(scope, operation_id)` identity covers the complete immutable request.
Equal acceptance returns the stored IDs; changed content conflicts. A separate
permanent `reading_effects` marker and `reading_articles` increment commit
together. Two operation IDs for one article produce two reads; a duplicate
returns its first stored result, even after later reads increment the article.
The local handler propagates `complete_in_tx` in that same transaction. The
receiver stores independent `outbox` and `webhook` projections in another
database, and acknowledges only an established committed marker. Markers have
no TTL because retained failed jobs, new transport IDs and restores can replay
the operation after transport retention expires.

`tests/jobs/reliability.rs` exercises rollback, concurrent marker arbitration,
conflict and uncertain-COMMIT readback on real PostgreSQL. Existing jobs,
webhook and outbox suites retain their protocol, fencing and transaction proof.
The full source-template rehearsal is selected once on the existing integration
carrier, after committing the candidate:

```sh
RUN_JOBS_RELIABILITY_REFERENCE=1 ALLOW_HEAVY=1 make test-integration-db
```

The source-only driver is `scripts/tests/jobs-reliability-reference.py`. It
requires the exact candidate commit, the existing PostgreSQL/NATS Compose
carrier, and its own PG image's dump/restore tools. It reuses release binaries
across scenarios. The initialized-service exercise starts at baseline
`ac88395be87cba3a1e0587f533dc50a71e358c8d`, installs and runs the feature first,
then uses normal portable sync plus an explicit scoped runtime source patch.
It verifies preserved business source/schema, durable data and service
customization by running the feature and recovery on the updated executable.
The driver refuses unexpected patch conflicts. Initialization/build durations
are recorded separately from runtime.

Each load scenario accepts 128 operations with at most 384 initial queue rows
and operation payloads at most 1 KiB, then stops producing. The worker uses
three ordinary slots plus the outbox publisher's one slot and an eight-slot
pool. Baseline, withheld worker, actual shared-pool pressure and NATS outage
are observed separately. Faults last at most five seconds; recovery has 180
seconds from release, including readiness and natural lease expiry. The whole
runtime scenario has 300 seconds including child shutdown. A violated bound
fails the run. Samples retain RSS/CPU, pool use/waits, admission/completion
gauges and distinct queue states; these are measurements, not a capacity claim.

The recovery sequence kills and waits the recorded child, takes an actual
producer backup with snapshot membership, and restores it to an empty database.
All old writers and consumers are stopped and joined before replacement.
Independent receiver markers survive outside that backup. An acceptance after
the snapshot is reported as the RPO gap if absent after restore. Every old
operator command/receipt is discarded, even if its numeric identity/version
matches again; a fresh inspection and effect readback determine the action.
Unavailable receiver truth leaves the isolated scenario in
`pending_manual_reconciliation` with processing stopped and no blind replay.
Positive cases check one durable effect per operation/channel, including new
transport IDs after actual retention cleanup and permitted operator redrive.

Receipts, milestones, source/binary/backup hashes, reconciliation and cleanup
diagnosis are retained under `target/jobs-reliability-reference/`, including
failed runs. Queue completion and transport ACK are reported separately from
durable business readback. The reference proves its disposable local recovery
mechanics; production restore guarantees depend on the actual backup contents.
<!-- template:end jobs-reference:test-readme-reading-reference -->
