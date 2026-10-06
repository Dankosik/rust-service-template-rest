# T2 — Successful JWKS acquisition time

Outcome:
Make the time of the most recently admitted usable key set visible through the
single unlabelled R2 gauge, while retaining the existing key and refresh policy.

Consumes:
- [Specification R2](../spec.md#r2-when-usable-jwks-material-was-last-obtained).
- [Design JWKS](../design/design.md#jwks) and its
  [schema](../design/design.md#metric-schema-and-material-flows) — timestamp
  representation, gauge custody and exact successful-install boundaries.

Provides:
- Owner-held gauge handle updated at admitted startup/replacement, owner
  regression coverage, and documented absence/reset/wall-clock semantics.

Boundary:
`KeyStore` remains sole writer. A failed/cancelled acquisition does not update
the sample; successful reacquisition samples current wall time even with the
same keys. No monotonic fabrication, extra timestamp state, issuer/key label,
clock framework, policy reader, task or config. Preserve existing refresh
counter semantics and usable-key retention. Acquisition time is not issuer
freshness, expiry, revocation or a readiness rule.

Mutable owners:
- `crates/infra-bearerauthn/src/refresh.rs` and its existing owner tests.
- During final-validation clone repair only: the existing `cfg(test)` recorder
  and narrowly needed test visibility in `src/jwt.rs`, to reuse its current
  Diagnostics owner. Production JWT behavior and interfaces remain outside this
  expanded mechanical repair scope.
- `docs/authentication.md`: OIDC JWT/provider operations guidance.

Exclusive locks:
- JWKS key-store owner and authentication guide; disjoint from other units.
- Final clone repair holds only the test-recorder portions of `jwt.rs` and
  `refresh.rs`; no other task currently writes those files.

Final validation:
- Claim: R2 reflects only successful usable acquisition and stays observational.
- Checks: Shared local completion route and final delivery review in
  [tasks.md](../tasks.md); executor selects focused tests and commands.
- Observable: Startup/replacement and non-success paths expose the accepted
  timestamp presence/value semantics while current keys and policy remain
  correct. Equal or backward wall-clock samples are not treated as failure.

Reopen if:
Design owns multiple concurrently configured key stores, different custody or
a necessary new dependency; Specification owns any changed staleness or signal
meaning. Routine time conversion/test choices stay with the executor.
