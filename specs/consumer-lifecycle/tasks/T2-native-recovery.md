# T2 — Source-only native durable recovery rehearsal

Outcome:
The source repository contains one finite executable rehearsal and operating
route that uses actual historical/current actors and native PostgreSQL/NATS
archives to observe jobs custody activation and restored durable work. This
turns the accepted operating design into reusable source-only proof without
shipping test machinery in consumer images.

Consumes:
- [D1–D4](../spec.md#d1-mixed-version-admission) — required observed outcome.
- [Release/recovery design](../design/release-recovery.md#local-consumer-and-version-preparation),
  its [ordered gates](../design/release-recovery.md#ordered-operational-gates)
  and [reconciliation](../design/release-recovery.md#recovery-custody-and-reconciliation)
  — fixed historical pair, finite topology, fencing and native restoration.
- [Ownership](../design/ownership.md#files-added-or-materially-changed-rust)
  and its surface map — existing proof package, harness and routing owners.
- Admitted `2cb871895b9edd018205fc98223477e269fce2e9`; historical
  `67be869acea112af271ec8ba621cbc50ae9d36b7` objects are execution inputs,
  each with its own full initializer/lock/toolchain.
- Suitable existing Docker, architecture and native backup-tool capacity gate
  rehearsal execution at Completion; they do not gate authoring.

Provides:
- Source-only rehearsal carrier, compatible historical actor/fixture source,
  explicit heavy invocation route and user guide.
- A fixed implementation capable of producing the separate D1–D4 Completion
  receipt and of supporting the published-worker observations defined by Design.

Boundary:
Implement Design's historical pre-custody→corrected transition, missing-history
refusal, bounded overlap, stop of all old retention owners and refusal of old
admission once custody is required. Implement finite fencing, native archive
validation, empty isolated restore, per-logical-ID reconciliation and corrected
readmission. Keep production data/RTO/RPO and service policy outside this drill.
Use the identical recorded source-only overlay for the historical pair; do not
patch their production sources/dependencies or add consumer migrations to the
embedded set. Historical rollback is not successful release rollback. Reuse
existing harnesses, dev dependencies, cleanup and native provider operations;
no generic recovery framework, provider upgrade, runner or shipped entrypoint.

Mutable owners:
- `scripts/ci/consumer-lifecycle-check.sh`, `test/tests/consumer_lifecycle.rs`,
  `test/examples/consumer_lifecycle_actor.rs` in the existing proof package.
- Existing source-only inventory and validation routing in
  `scripts/lib/template_profiles.json`, `make/source.mk`, `make/template.mk`,
  `scripts/ci/changed-surfaces.sh`, associated self-test and `verify.sh` only
  where current owners require parity; no new ordinary CI restore matrix.
- `docs/consumer-lifecycle-rehearsal.md` and links in existing
  jobs/messaging/production/validation owners.

Exclusive locks:
- Source-only inventory/source-validation routing shared with T1/T3; serialize
  overlapping edits. The Rust fixture and guide can proceed independently.
- Existing proof-package shared helpers only if a necessary mutation is found;
  update the lock before that mutation, rather than inventing a helper owner.

Final validation:
- Claim: Actual native execution establishes D1–D4 for the exact pair and
  synthetic topology, preserving restored jobs/history/sequence, outbox intent,
  stream/DLQ/durable state and one durable effect per logical ID after replay.
- Checks: Consolidated affected Rust/source/script checks and the explicitly
  requested native rehearsal once in the existing suitable runtime, as required
  by [Design](../design/release-recovery.md#smallest-deployment-graph).
  Implementation chooses cases/commands. External-input Rust scenarios remain
  ignored by ordinary discovery; the explicit target requires their actual
  execution/completion, so a skip is no pass.
- Observable: Actual old/new process identities, schema admission, custody
  activation, immutable native archives, restored durable identities, replay
  settlement and measured recovery intervals. Report tool/server versions and
  unresolved state. No inference of production durability or RTO/RPO.

Reopen if:
Historical API/fixture compatibility, selected native backup format/topology
or bounded custody/reconciliation mechanism cannot meet Design: return that
Technical Design owner. Missing required final runtime evidence remains
incomplete through the parent, without blocking unrelated coding.
