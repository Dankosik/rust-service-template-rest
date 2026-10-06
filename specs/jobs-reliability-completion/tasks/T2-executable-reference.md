# T2 — Executable durable-effect recovery across a real template upgrade

Outcome:
A reproducible reading-counter reference exercises acceptance through the real
worker and independent receivers, demonstrates bounded recovery and deliberate
unknown hold, and keeps its working feature and owned data across an actual
baseline-to-candidate template upgrade.

Consumes:
- [Specification](../spec.md), R2–R5 and joint finality — complete accepted reference/recovery outcome, including mandatory live evidence and the HeaderValue prose correction.
- [System design](../design/system.md#r2r3r5-one-business-flow-and-its-authorities), [crash/restore](../design/system.md#r2-crash-restore-and-operator-sequence), [workload](../design/system.md#r4-fixed-workload-budget-and-observation), [upgrade](../design/system.md#r5-exact-initialized-service-upgrade) and [carrier](../design/system.md#carrier-proof-boundaries-and-release-closure) — closed mechanisms and proof boundaries.
- [Ownership](../design/ownership.md) — fixture, schema, receiver, driver, docs and source-only invocation owners.
- T1 Implemented runtime paths and gauge identities — gate only the final measurement binding and exact patch-adoption wiring. Earlier reference implementation is ready from the accepted contracts. Passing T1 proof gates only assembled acceptance.
- Exact committed candidate SHA — gate R5 execution at final validation, not fixture/driver authoring. Baseline is `ac88395be87cba3a1e0587f533dc50a71e358c8d`.

Provides:
- Fixture-owned atomic acceptance, marker/aggregate recipe, independent channel-scoped receiver and local CLI using actual `jobs_worker::run`.
- Combined scenario and driver supporting real crash/dump/restore, reconciliation, transport cleanup/redrive, bounded measurements and executed initialized-service upgrade; truthful invocation and ownership documentation.

Boundary:
Keep R2–R5 as one consumable recipe. Implement the accepted three-intent
transaction, separate no-TTL marker and aggregate, same-identity/conflict rules,
caller-owned Tx/unknown handling and independent outbox/webhook receiver truth.
Implement the accepted crash/restore/operator sequence, including snapshot RPO,
discarded stale recovery commands and unknown readback hold without blind replay.
The fixed workload remains 128 operations, at most 384 initial rows, payload
at most 1 KiB, three ordinary plus one publisher slot, pool eight and the
5/180/300-second fault/recovery/runtime envelope. Setup is separately timed;
failed bounds are failed scenarios, never silently enlarged or skipped.

Upgrade uses an initialized baseline service with an already-working,
service-owned reference and customization, normal portable sync, then explicit
scoped adoption of T1's runtime patch. Preserve business source/schema/data and
independent receiver store; run feature and R2/R3 behavior after upgrade. Keep
the standalone fixture compatible with baseline public APIs. No shipped
business migration, public REST surface, dependency upgrade, cloud runner or
replacement process framework. Preserve adequate existing protocol/sync proof.

Mutable owners:
- `integration-tests`: `test/src/reading_counter.rs`, `reading_counter_receiver.rs`, `bin/reading_counter_fixture.rs`, profile-gated `lib.rs`, scoped `test/Cargo.toml` declarations, `test/fixtures/migrations/reading_counter/`, `test/tests/jobs/reliability.rs` and its `main.rs` module wiring.
- Existing reusable test seams only when needed by this reference and outside T1's `execution.rs`/`process.rs` scope; coordinate any actual overlap before editing.
- `scripts/tests/jobs-reliability-reference.py`; the existing source-only integration command/classifier/CI carrier if required to invoke it once within the current integration job.
- Relevant jobs/async/outbox/webhook guides and test README, including lifecycle-facing guidance from T1 and the narrow stale HeaderValue wording fix. Preserve the implemented HeaderValue contract.
- Disposable derived-service application and fixture/schema/customization material only as created by the driver during final validation; its paths/resources have one run manifest and lifecycle owner.

Exclusive locks:
- Integration-test manifest/module wiring and reference schema/fixture; reference driver and source-only integration command/classifier; the amended shared guides. T1 production Rust and narrow jobs regressions stay disjoint.
- Existing shared validation lock is acquired only for later validation; fixture runtime resources are run-scoped and owned by the driver. Coding does not reserve them.

Final validation:
- Claim: R2–R5's positive branches establish one committed effect per logical operation/channel, while unavailable effect truth remains unknown; observations survive producer restore and feature upgrade without claiming generic exactly-once transport delivery.
- Checks: The ledger's matching build/tests and required real-PG/NATS combined reference exercise on the existing carrier, once for source and one initialized representative; actual distinct revisions and executed sync/adoption; static docs/changed-carrier checks; selected exact-head CI and final assembled review. Implementation chooses cases/assertions/commands, reuses compiled binaries and adequate existing proof, and records the execution entry point for the delivery owner.
- Observable: Durable acceptance, queue and effect readbacks remain distinct; real child exit and backup/restore are evidenced; rollback/concurrent duplicate/conflict/unknown and transport replay preserve the recipe; 128 positive operations drain within bounds, actual admission/bookkeeping return idle, pool remains within eight, and RSS/CPU are reported as measurements. Receipt includes exact revisions/binary/source identities, resource/config scope, backup membership, milestones, reconciliation decisions, upgrade preservation and actual cleanup, including failed-run diagnosis.

Reopen if:
Reference APIs, fixture placement, carrier availability or measured envelope
contradict the accepted mechanism: Technical Design. Changed finality or replay
meaning: Specification. New external authority: Intent/continuation owner.
An unavailable required runtime proof leaves that final claim incomplete while
independent code work continues; it never converts a real-run requirement into
a source-only success.
