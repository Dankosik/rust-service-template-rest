# T2 — Bound SDK response collection

Outcome:
Every current nonstreaming S3 response and GET error is limited to 1 MiB actual DATA before SDK collection, with existing SDK parsing and lost-response outcomes.

Consumes:
- [S2](../spec.md#s2-bound-sdk-nonstreaming-response-collection) and [selected S2 mechanism](../design/selected-design.md#s2-sdk-collection-envelope-before-parsing).
- [R2 and R9 owners](../design/ownership.md#files); resolved SDK source anchors in selected design.

Provides:
- Client error-response and four control-success interceptors, sharing the native limiter, and accurate storage-envelope guidance.

Boundary:
Private response_limit.rs composes supported SDK Intercept, MapFrame and Limited. Wire the client non-2xx hook before SDK checksum interceptors and 2xx hooks for PutObject, HeadObject, DeleteObject and HeadBucket. Count DATA, not HEAD metadata; preserve successful GET, once mutation overrides, native parsing/retries and Unavailable/OutcomeUnknown classification. No new dependency or custom parser.

Mutable owners:
- infra-object-storage response owner: crates/infra-object-storage/src/response_limit.rs, crates/infra-object-storage/src/lib.rs and existing crates/infra-object-storage/src/tests.rs.
- docs/object-storage.md: nonstreaming ceiling and outcome/lifetime scope, preserving T1's changes.

Exclusive locks:
- Storage fixture/guide: crates/infra-object-storage/src/tests.rs and docs/object-storage.md, shared with T1; serialize these task writers. No manifest lock.

Final validation:
- Claim: interception covers every accepted operation/status path before collection and preserves SDK error/checksum/outcome semantics.
- Checks: existing local criterion once at assembled Completion; executor chooses cases/commands in current fixtures. No live bucket or new runner requirement.
- Observable: actual-body ceiling and stable SDK/public classification; successful GET remains on the prior streaming contract.

Reopen if:
Pinned hook order, operation inventory or checksum behavior contradicts selected design: Technical Design; changed cap/outcome: Definition.
