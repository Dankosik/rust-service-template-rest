# Definition evidence: current messaging and feedback boundaries

Valid as of 2026-10-06. This is supporting Definition research, not a runtime
verification or a mechanism decision. The question was which approved behavior
gaps remain on current main, and which current guarantees must be preserved.
The stopping condition is enough primary evidence to close that behavior delta;
capacity, fault execution and root-cause experiments belong to subsequent work.

## Candidate and authority

Current checkout base: `699887b18594088a59bcc23a049d290d089f6da1`, including
runtime/task-custody PR #254 (`468353341db221741cffc05194053f49ec2c4779`) and
build-speed PR #255. The separate earlier delivery candidate is
[PR #239](https://github.com/Dankosik/rust-service-template-rest/pull/239),
head `7223ea877f031d440842d3df6876857e91492ec2`; it is not part of this base.
The coordinator checked it open and behind main. Its previous passing CI is
historical evidence for its own scope, not a pass for this expanded candidate.

The task's [intent](../intent.md) owns accepted requester meaning. Current
[messaging](../../../docs/durable-messaging.md),
[outbox](../../../docs/postgres-transactional-outbox.md),
[persistence](../../../docs/architecture/persistence.md#transaction-truth) and
[build-speed](../../../docs/build-speed.md) owners ground unchanged behavior.

## Admission and uncertainty

Fact: `admit_topology` in
[`messaging.rs`](../../../crates/infra-messaging/src/messaging.rs) reads source
configuration, checks server limits and consumer source message size, resolves
a distinct DLQ stream and reads it. It does not check `no_ack`, DLQ message-size
compatibility or file/default persistence. PR #239's exact diff adds the last
two storage-mode refusals (memory and asynchronous persistence), but neither
`no_ack` nor DLQ transfer-size admission. Thus its storage work is a required
integration input, not grounds for claiming the remaining gaps are closed.

Fact: the locked and locally repaired async-nats 0.50.0 exposes `no_ack`,
`max_message_size` and `persist_mode` in
[`stream::Config`](../../../vendor/async-nats/src/jetstream/stream.rs).
The retained broker image is NATS 2.15.0 in
[`docker-compose.yml`](../../../env/docker-compose.yml). Official
[NATS 2.15.0 stream source](https://github.com/nats-io/nats-server/blob/v2.15.0/server/stream.go#L6483)
sets publication acknowledgment behavior from `!NoAck` (confirmed against the
tagged raw source). The adapter's positive-PubAck contract therefore cannot be
satisfied by a `NoAck=true` stream. It may still accept bytes; a client timeout
does not prove absence. This makes admission rejection necessary and preserves
the existing ambiguous-outcome rule for later configuration drift.

Counter-evidence: rejecting every unusual broker option is unnecessary. This
task closes the named ACK, storage and transfer-bound incompatibilities only;
it does not infer that all operator-selected retention or replication choices
can be certified by the adapter.

## Current runtime custody and DLQ

Fact: main #254 added shared native publication admission and bounded ACK
cleanup, cooperative outbox preparation, and lifecycle custody. Its consumer
DLQ path now builds a native publication including expected-stream metadata.
The adapter's existing header and publication-byte bounds remain active inputs.
The current docs still retain one publisher slot, a shared process failure
domain, shared absolute teardown budgets, and broker-default `MaxAckPending`.
These improvements do not measure outbox/R3/TLS capacity or implement role
separation. Existing component/R1 measurements explicitly exclude those claims.

Fact: [`restore_dead_letter`](../../../crates/infra-messaging/src/wire.rs)
returns a `PreparedEvent`; the deterministic `redrive-` publication identity
uses the dead-letter record's stream, sequence, timestamp and original
publication identity. It neither performs operator inspection nor publication
and record retirement. Jobs recovery commands operate on PostgreSQL job rows,
not broker DLQ records. Recovery must retain actual DLQ coordinates; source
coordinates carried in transfer context are not interchangeable.

Inference: an adopter needs a controlled end-to-end action for one record, but
does not need a new persistent consumer/delivery engine. Technical Design must
choose how native inspection and a wire-aware action compose.

## Recovery feasibility and limits

Fact: official [NATS stream backup/restore documentation](https://docs.nats.io/learn/backup-recovery/stream-backup-restore)
describes native snapshots containing stream messages/configuration and consumer
state, then restoration into a stream that does not already exist. This is an
available building block; a stream snapshot is not an atomic backup across
producer PostgreSQL and the consumer's effect database.

Coordinator readback: Docker 29.4.0, native NATS CLI v0.5.0, `psql`, `pg_dump`
and `hyperfine` are available; CLI help exposes native backup, restore and
validate commands. No recovery operation was run in Definition. Exact commands,
temporary-resource ownership and fixture compatibility need design/execution
verification. Local 16 GiB host load is not performance evidence.

The [production contract](../../../docs/production-contract.md) leaves service
RPO/RTO and recovery proof unresolved. Template rehearsal must expose mismatched
restore consequences and exact missing identities without filling those service
decisions. Counts alone can agree while the wrong event/effect survives.

## Developer feedback drift

Fact: current
[`the_span_names_the_region_only_on_amazon`](../../../crates/infra-object-storage/src/tests.rs)
uses a test-local tracing subscriber plus another live dispatcher. The related
test explains process-wide callsite interest. #254 changes body/fixture tests
but leaves this region-span test untouched. The earlier 0-versus-1 CI failure
and unchanged passing rerun are coordinator-supplied observations; Definition
does not identify the cause or claim it still reproduces. A dispatcher race is
a lead, not an established root cause.

Fact: [`validation-lock.sh`](../../../scripts/ci/validation-lock.sh) shares a
Git-common lock, stores PID/candidate/command, reclaims an absent PID, waits up to
900 seconds by default and prints ownership only on timeout. It does not emit
ordinary wait/owner progress or identify the owner's worktree. #255 adds scoped
type-check/build guidance in `docs/build-speed.md`; it does not change the lock.
This supports the remaining wait-diagnostic delta, not a new machine setup task.

## Refresh and remaining owners

Refresh this bounded baseline if main/PR #239 integration changes admission,
native publication custody, consumer settlement, object-storage tracing, or
validation locking. Technical Design owns mechanism/library comparison,
rehearsal composition and feasibility, empirical decisions about capacity and
role/concurrency knobs, and the focused flake investigation strategy. Execution
owns selected concrete tests and results. No required requester meaning remains
unknown at the Definition boundary.
