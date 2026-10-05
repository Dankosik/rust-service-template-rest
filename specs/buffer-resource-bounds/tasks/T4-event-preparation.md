# T4 — Bound retained event JSON

Outcome:
PreparedEvent serializes once with bounded retained output, preserving successful bytes, existing Envelope errors/precedence and outbox payload encoding.

Consumes:
- [S3](../spec.md#s3-bound-json-preparation-without-changing-accepted-json-or-errors), [selected writer mechanism](../design/selected-design.md#s3-one-serialization-bounded-retained-output), [R4/R9 owners](../design/ownership.md#files).
- Closed existing jobs-ceiling and padded-base64 outbox contract; T3 implementation is not a coding prerequisite.

Provides:
- A private messaging-only writer and accurate preparation/backing/base64 explanations in the outbox guide.

Boundary:
Retain counting/discarding in prepared.rs, preserve serialization-before-size-before-identity/time checks and wire format. Prepared clones remain caller-owned. No jobs-only dependency, domain-events wire policy or outbox storage change.

Mutable owners:
- infra-messaging preparation: crates/infra-messaging/src/prepared.rs and existing crates/infra-messaging/tests/wire_compat.rs if needed for this outcome.
- docs/postgres-transactional-outbox.md: event serialization retention, shared prepared backing and existing base64-plus-metadata ceiling. Publication admission/lifetime guide remains T6-owned.

Exclusive locks:
- none. T6 owns different messaging files/fixtures and docs/durable-messaging.md.

Final validation:
- Claim: bounded preparation retains the same payload/error contract and outbox encoding under removable jobs/messaging profiles.
- Checks: ordinary local criterion at assembled Completion; executor selects cases/commands, without new database or broker infrastructure.
- Observable: preparation output/errors and existing encoding parity; source/outbox publication behavior remains T6's claim.

Reopen if:
Wire ownership or a new shared owner is required: Technical Design; Envelope/precedence change: Definition.
