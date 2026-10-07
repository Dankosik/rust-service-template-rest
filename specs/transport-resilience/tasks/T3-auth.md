# T3 — Authentication-provider connection sub-budget

Outcome:
Discovery, JWKS and introspection gain the native two-second connection
deadline within their unchanged three-second total request/body budget, so a
stalled connect does not strand later provider attempts.

Consumes:
- [Specification](../spec.md#authentication-provider-dial-budget).
- [Design](../design/transport.md#grpc-and-authentication) — fixed native timeout and retained provider policy.
- [Execution boundary](execution-boundary.md).

Provides:
- Shared auth-provider client setting, distinguishing regression coverage and accurate auth-provider transport guidance.

Boundary:
Keep native candidate handling, unavailable classification, total/body expiry,
no proxy/redirect/retry, origin/body limits and cache/bulkhead semantics.
Outbound OAuth remains with its existing outbound HTTP owner. No new knob.

Mutable owners:
- `crates/infra-bearerauthn/src/provider.rs` and existing provider tests/fixtures.
- `docs/authentication.md` for connect/total bounds and client/trust snapshot limits.

Exclusive locks:
- none.

Final validation:
- Claim: Stalled connection ends within its sub-budget while fallback/subsequent attempts remain usable and the three-second total still covers body completion.
- Checks: Matching assembled build/tests and documentation route under execution-boundary.md; no additional checks.
- Observable: Native connect failure releases the attempt through current failure semantics, without an extended request or relaxed trust/body policy.

Reopen if:
The resolved native client cannot supply the selected semantics (Technical
Design); accepted provider behavior requires different policy (Definition).
