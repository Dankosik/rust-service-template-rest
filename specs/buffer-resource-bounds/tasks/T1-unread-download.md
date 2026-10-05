# T1 — Collect only the unread download

Outcome:
Replace original-object collection reservation with the unread remaining bytes plus held final chunk, including zero reservation for an exhausted tail, while preserving the existing completion state machine.

Consumes:
- [S1](../spec.md#s1-collect-only-an-unread-object-tail) and [selected S1 mechanism](../design/selected-design.md#s1-unread-download-collection).
- [R1 and R9 ownership](../design/ownership.md#files); [S6](../spec.md#s6-correct-and-complete-the-adopter-facing-explanation) storage lifetime qualifications.

Provides:
- Correct unread collection through the existing public Download; guide distinguishes stream admission from completed returned Bytes and slow/unpolled ownership.

Boundary:
Use remaining plus held last; keep checksum/EOF, stable repeated failure, metadata, final-chunk hold and permit release. No blanket Bytes copy/shrink, upload change or universal response policy.

Mutable owners:
- infra-object-storage download owner: crates/infra-object-storage/src/download.rs and its existing adjacent tests.
- Shared storage fixture owner: crates/infra-object-storage/src/tests.rs only as needed for this outcome.
- docs/object-storage.md: unread collection and lifetime/backing-capacity guidance only.

Exclusive locks:
- Storage fixture/guide: crates/infra-object-storage/src/tests.rs and docs/object-storage.md, shared with T2; serialize these task writers. No manifest lock.

Final validation:
- Claim: resumed/partial/exhausted collection retains the right bytes and bounded requested capacity with existing terminal ownership.
- Checks: existing local criterion once at assembled Completion; cases and commands chosen by executor; no added proof gate.
- Observable: exact returned unread bytes and unchanged terminal/error/permit contract at the current local boundary; no measured RSS claim.

Reopen if:
Unread state cannot represent the required collection, or EOF/checksum/error ownership would change: Technical Design; changed behavior: Definition.
