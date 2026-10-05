# T4 — Admit Redis TTL arguments before SET

Outcome:
Replace cache SET's minimum assertion and oversized saturation with a typed
InvalidTtl result before command/observation construction. Admit exactly the
floored whole-millisecond range 1..=i64::MAX and preserve ordinary SET behavior.

Consumes:
- [Specification: Redis TTL admission](../spec.md#redis-cache-ttl-admission).
- [Design: Redis TTL input contract](../design/technical-design.md#redis-ttl-input-contract).
- [Cache API owner](../../../crates/infra-cache/src/lib.rs) and
  [cache guide](../../../docs/cache.md).

Provides:
- Public SetError distinguishing InvalidTtl from Unavailable, matching callers
  and accurate cache TTL/error guidance.

Boundary:
Keep set's new Result<(), SetError>, declared thiserror support and sanitized
messages in infra-cache. Admit the u128 whole-millisecond value before converting
to existing PX storage. Invalid TTL must cause no dispatch, connection retirement
or fabricated outage metric. Transport/server refusal remains Unavailable with
one dispatch, existing timeout ambiguity and no retry. Redis owns absolute
expiry arithmetic; do not estimate it using host time. Update actual in-scope
set callers, examples, rustdoc and superseded panic expectations as one unit.
Preserve get/delete/probe, namespace validation and the shared Unavailable type.

Mutable owners:
- `crates/infra-cache` SET API, related caller/test code and rustdoc; manifests
  remain unchanged.
- `docs/cache.md` TTL/error contract and examples; `docs/cache-decisions.md`
  only where an existing SET contract statement needs consistency.
- Any actual repository SET caller requiring the changed error type, within
  this semantic API migration; reconcile with the root before writing outside
  infra-cache if another Lead owns the surface.
- This packet's implementation details and chosen final-validation commands.

Exclusive locks:
- none.

Final validation:
- Claim: Invalid floored TTL arguments are rejected locally and distinctly;
  admitted input keeps its exact floored PX value and existing command failure
  semantics. All in-scope consumers match the typed SET contract.
- Checks: The ledger's consolidated matching build and relevant tests plus
  documentation consistency. Existing CI-owned cache integration stays in CI;
  no new service or runtime proof is required. The Lead selects concrete cases
  and commands while implementing.
- Observable: SET result and predispatch admission boundary. Local argument
  admission does not claim Redis will accept server-now plus that TTL.

Reopen if:
System Design if the typed error or resolved PX API cannot preserve the accepted
contract; Specification for a new maximum retention policy or retry behavior.

## Implementation result

```text
unit: T4
verdict: Implemented
candidate: bounded working-tree diff in crates/infra-cache/src/{lib,tests}.rs, docs/cache.md, docs/cache-decisions.md, and this packet
provides: SetError and pre-observation whole-millisecond TTL admission, migrated SET error consumers/examples, protocol boundary coverage and consistent guidance; unverified
next_owner: root final-validation owner after every implementation writer releases its scope
```

`SetError` preserves sanitized runtime `Unavailable` through its wrapper and
distinguishes local `InvalidTtl`. SET computes u128 whole milliseconds, admits
`1..=i64::MAX`, and only then converts to the redis-rs 1.7.1 `PX(u64)` argument.
GET, DELETE, probe, connection handling, manifests and policy bounds are unchanged.
There are no production SET consumers outside infra-cache; existing integration
SET consumers unwrap success and need no signature edit.

The obsolete sub-millisecond panic test is replaced by one owner-boundary
protocol test using the existing FakeServer and metrics recorder. Invalid
cases are zero, 999999 ns, maximum plus 1 ms, and Duration::MAX. They must
produce InvalidTtl without SET dispatch, replacement connection or SET metric.
Admitted cases are 1 ms, 1999 us, 27 ms, maximum whole milliseconds, and maximum
plus 999999 ns; independent decimal wire expectations preserve flooring.
Existing READONLY/recovery and stalled-write/no-replay tests now require
`SetError::Unavailable(Unavailable)`. No production test seam was added.

Chosen consolidated final commands: `make build` and `make test` for the
assembled multi-crate candidate, plus `make docs-check` for changed guidance.
The ordinary workspace test command includes infra-cache unit tests and its
compiled doctest. If the root routes affected packages instead, include
`infra-cache` in `make test-changed PKGS="..."`; no second cache-only suite is
needed. Bounded signature diagnostics can use
`cargo check --locked -p infra-cache --all-targets`, serialized by the root.
Existing Compose cache integration remains CI-owned.

Only rustfmt was run during implementation. No build, compiler diagnostics,
test, regression red/green run, or independent review was run for this packet.
The protocol fixture proves submitted arguments, not Redis server-time expiry
arithmetic; server refusal retains unavailable semantics. Implementation scope
is released, and its Lead remains available for anchored final-validation repairs.
