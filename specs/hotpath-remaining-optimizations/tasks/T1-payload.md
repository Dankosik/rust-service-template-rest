# T1 — Preserve webhook payload meaning with streaming Base64 serialization

Outcome:
Replace Incoming's per-field temporary encoded Strings with the accepted
private Serde/Base64 adapter while preserving exact serialized JSON bytes,
generic preparation errors/order, duplicate bypass and bounded request storage.

Consumes:
- [Specification](../spec.md#preserved-behavior-and-truth) — payload, admission and durable invariants.
- [Payload design](../design.md#payload-mechanism-and-flow) and [ownership](../design.md#ownership-map) — closed mechanism and placement.
- [Ledger baseline custody](../tasks.md#carrier-and-ready-frontier) — immutable pre-edit source.

Provides:
- A bounded independently removable Incoming serialization delta, including any material gaps in existing-owner tests; generic jobs and SQL remain unchanged.

Boundary:
Use private SerializeAs over Base64Display/STANDARD and collect_str, with split
serialization/deserialization annotations on message_id, body and optional
content_type. Preserve derived layout and current Base64 decoding. No capacity
hint API, custom encoder, second pass, buffer cache, manifest/schema change or
jobs ownership transfer. Keep the accepted body transfer. Implementation
chooses and writes tests alongside code, preserving unrelated existing edits.

Mutable owners:
- Incoming serialization and existing in-file tests in `crates/infra-webhooks/src/inbound.rs`.
- Existing webhook black-box behavior tests in `test/tests/webhooks/inbound.rs`, only for a material uncovered accepted invariant.

Exclusive locks:
- none

Final validation:
- Claim: the assembled retained adapter preserves exact payload bytes and admission/durable behavior, and qualifies as a measured allocation optimization under Specification.
- Checks: consolidated ledger Completion; relevant existing-owner build/tests and required real-PostgreSQL proof, exact byte/error parity, matched total allocation and ordinary-release controls. Concrete cases and commands belong to Implementation; no check runs merely to close T1.
- Observable: lower total allocated bytes per newly admitted delivery, allocation count and CPU separately; no reproducible ordinary regression, duplicate preparation or behavioral divergence. A rejected adapter is removed and its unfavorable evidence retained; area disposition remains explicit.

Reopen if:
The selected APIs cannot preserve bytes/errors/bounds, or output-growth cost
requires the deferred generic capacity mechanism: Technical Design. Changed
behavior/adoption meaning: Definition. Missing required remote evidence:
root/delivery at Completion; it does not block writing the closed candidate.
