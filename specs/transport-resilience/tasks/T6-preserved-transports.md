# T6 — Accurate guidance for preserved outbound HTTP and Redis

Outcome:
Operator guidance describes the actual DNS, pool/socket and trust-material
lifetimes of unchanged outbound HTTP and Redis transports, removing unsupported
live-migration or global hot-reload implications.

Consumes:
- [Specification unchanged surfaces](../spec.md#deliberately-unchanged-and-documentation-corrections).
- [Design proof/operational statements](../design/transport.md#proof-and-operational-statements).
- Current provider source as factual authority; [execution boundary](execution-boundary.md).

Provides:
- Source-grounded outbound HTTP and Redis guidance, preserving adequate native candidate/race/recovery behavior.

Boundary:
Documentation only. Distinguish DNS on new dials from busy socket migration and
idle eviction from maximum lifetime. State actual client/trust construction,
platform-verifier limits and restart/reload boundary without global hot-reload
claims. Redis retains candidate racing and its owned generation/recovery path.
Do not change executable code, APIs, policies, or create new runtime proof requirements.

Mutable owners:
- `docs/outbound-http.md` and `docs/cache.md`.
- The matching TLS configuration comment in `crates/infra-outbound-http/src/lib.rs`; documentation-only correction.

Exclusive locks:
- none.

Final validation:
- Claim: Operator statements match current source and the preserved behavior accepted by the design.
- Checks: Assembled static consistency review and docs-check; no additional checks.
- Observable: Guidance makes no unsupported lifetime/rotation promise and links to the current owners.

Reopen if:
Source contradicts an accepted preserved mechanism (Technical Design), or a
new transport behavior is required (Definition); do not fix it through prose.
