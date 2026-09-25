# Definition transition

status: ready

owner: Definition (Intake, supporting Research, Specification)

result: [intent.md](intent.md), [spec.md](spec.md),
[research/synthesis.md](research/synthesis.md)

review: [definition-review.md](definition-review.md), fresh PASS for the
original-request introspection-cache correction from native reviewer
`/root/authn_definition/cache_definition_review`; prior PASS evidence remains
valid for unchanged scope.

movement_evidence: Reviewed Intake/R7 now distinguish required cache delivery
from optional activation. Positive verified results are bounded by token/trust
context, capacity, fixed retention and exp, with no negative/error/stale cache.
Disabled behavior is preserved. No user-owned question survives. Current
production candidate `a896e21f27acffc03193e6d43613c6e5173603b0`; no production
edit or build is part of this Definition repair. Independent prior R1–R10
decisions remain unchanged; current no-cache downstream wording is superseded
only for this delta and must be reconciled by its next owner before execution.
`make docs-check` passed with zero link/fragment errors after the repair.

reopen_owner: none. No user-owned question or new approval is required.

next_owner: Technical Design, narrow reopen. Close only the cache's
mechanism, finite configuration bounds, ownership and affected projections;
preserve all independent accepted decisions. The root coordinator owns dispatch
and later PR/CI delivery.

## Proof and authority boundary

Reuse pinned dependencies and existing adequate tests. Implementation chooses
concrete tests and final-validation scheduling; no new test-design phase or
multiplied build matrix is created. Final delivery needs independent review for
authorization/concurrency changes. Commit, push and separate PR are authorized;
merge, deployment, live-provider certification and independent Stage 10.2
behavior changes are outside scope.
