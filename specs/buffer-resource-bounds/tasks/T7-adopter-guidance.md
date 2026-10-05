# T7 — Explain feature-owned resource lifetime

Outcome:
Existing adopter guidance accurately distinguishes byte/count/backing/lifetime limits and gives supported HTTP/gRPC feature recipes without adding universal runtime policy.

Consumes:
- [S6 and non-goals](../spec.md#s6-correct-and-complete-the-adopter-facing-explanation), [audit dispositions](../research/dispositions.md), [R9 guide ownership](../design/ownership.md#files).
- Existing unchanged HTTP/outbound/gRPC/auth behavior and [design S6](../design/selected-design.md#s6-documentation-and-proof-boundary); provider-specific changed-contract guidance is included in T1–T6.

Provides:
- One coherent adopter-facing explanation of feature-owned resource policies, using existing HTTP/gRPC APIs and accurate limitations of existing cache targets.

Boundary:
Explain retained backing versus visible length, request/handler versus response-result lifetime, wire versus decoded buffers and count/weight versus RSS. HTTP recipe holds admission through body completion/drop with finite payload and feature-owned slow-client/write lifetime. Document gRPC per-service/client send/receive setters, existing defaults/compression, aggregate stream/message count and caller lifetime; completed readers need prompt drop. Retain outbound HTTP's existing collector/trailer/deadline policy. Clarify auth cache wording only if it overclaims. No protocol/listener/business/config/runtime change or new example application.

Mutable owners:
- docs/architecture/http.md, docs/backend-utility-recipes.md, docs/grpc.md.
- docs/authentication.md and docs/outbound-machine-authentication.md only for demonstrated count/weight/RSS wording corrections; otherwise record unchanged disposition in the returned unit result.
- Existing fenced recipes in those files, preserving profile markers. Other crate doctests/examples remain unchanged unless a concrete mirrored-contract discrepancy is returned to the root for a mechanical exact-owner assignment before editing.

Exclusive locks:
- Listed guide files only; T1–T6 provider guides stay outside this scope.

Final validation:
- Claim: every remaining audit disposition has truthful scope and useful supported feature guidance, with no invented hard RSS or universal runtime guarantee.
- Checks: static consistency and existing documentation/example route once at assembled Completion; executor selects any necessary example checks. No new harness or benchmark.
- Observable: readable supported recipes and resolving links consistent with current source and preserved non-goals.

Reopen if:
Guidance would require a new runtime policy, business limit, unsupported API or different owner: Technical Design; changed requester behavior: Definition.
