# T3 — Bound retained job JSON

Outcome:
Enqueue and compare_live_payload serialize once with at most MAX_PAYLOAD_BYTES retained, preserving exact successful output and Serialize-before-size-before-NUL precedence.

Consumes:
- [S3](../spec.md#s3-bound-json-preparation-without-changing-accepted-json-or-errors), [selected writer mechanism](../design/selected-design.md#s3-one-serialization-bounded-retained-output), [R3/R9 owners](../design/ownership.md#files).

Provides:
- One private counting/discarding writer at jobs preparation and accurate jobs retention guidance, independently usable by the jobs profile.

Boundary:
Count all successfully serialized bytes with checked arithmetic while discarding excess; late serializer errors still win. Preserve SQL, transaction ownership, stored format and existing outbox padded-base64 accounting. No utility crate or messaging dependency.

Mutable owners:
- infra-jobs preparation: crates/infra-jobs/src/enqueue.rs including adjacent tests.
- docs/background-jobs.md: retained serialized bytes, exact errors and caller-owned serializer/result costs.

Exclusive locks:
- none.

Final validation:
- Claim: successful bytes and validation order stay identical while retained serialization is bounded; both existing prepare callers consume it.
- Checks: ordinary local criterion at assembled Completion; executor selects cases/commands. No new database-observation claim or database matrix.
- Observable: preparation result/error and exact oversize count before database effect.

Reopen if:
A shared owner or changed serialization/SQL contract becomes necessary: Technical Design; observable error change: Definition.
