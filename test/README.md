# Integration tests

This workspace crate (`integration-tests`) holds cross-crate utility-recipe and
adapter integration suites. PostgreSQL-specific proof runs only when the local
PostgreSQL profile is retained (`make test` compiles without the `integration`
feature so ordinary unit runs need no Docker; [`make/template.mk`](../make/template.mk)
and [PostgreSQL Validation](../docs/validation/postgres.md) own container-backed
invocations).

<!-- template:begin jobs-reference:test-readme-reading-reference -->
## Reading-counter recovery reference

`src/reading_counter.rs` and `src/bin/reading_counter_fixture.rs` provide a
reference workload retained only when jobs, PostgreSQL outbox, JetStream, and
durable outbound webhooks are all enabled; shipped service binaries expose none
of its routes or fixture schema.

`tests/jobs/reliability.rs` exercises rollback, concurrent marker arbitration,
conflict, and uncertain-`COMMIT` readback on PostgreSQL. Existing jobs, webhook,
and outbox suites retain their protocol, fencing, and transaction proof.
<!-- template:end jobs-reference:test-readme-reading-reference -->
