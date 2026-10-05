# T1 — Admitted inbound request diagnostics

Outcome:
Replace built-in HTTP raw request diagnostics and server gRPC caller identity
with only the accepted finite fields/correlation. This request-observation source
change is independently usable with the existing subscriber/provider. Panic
emission and recovery are wholly T2, because the shared hook runs first.

Consumes:
- [Specification S2](../spec.md#s2-diagnostic-privacy-at-the-source) and
  [S4](../spec.md#s4-operating-contract-and-compatibility): source privacy and compatibility.
- [Design source policy](../design/technical-design.md#source-privacy-and-sdk-diagnostic-admission): normalized HTTP method everywhere, remove query denylist and client/server gRPC role split.
- [Ownership H/G](../design/ownership.md#responsibilities),
  [inverse file map](../design/ownership.md#files): exact source/proof owners.

Provides:
- HTTP/gRPC server observation never first records excluded raw path/query,
  User-Agent or caller authority. Admitted route,
  normalized method, status/protocol/timing/correlation and gRPC client destination
  identity retain their meaning.
- Corresponding source-privacy operating text at the existing configuration
  policy owner for T2 to compose with the remaining shared telemetry changes.

Boundary:
Implement H/G; remove replaced raw capture/query denylist with no compatibility
alias. All P (HTTP recovery and shared hook/consumers), SDK diagnostic gate,
formatter/output, metrics and resource cleanup belong to T2. Preserve HTTP
Problem/status, gRPC behavior/status and existing finite labels/buckets/sampling.

Mutable owners:
- `infra-http` inbound observation source and existing inline proof;
  `infra-grpc` role-aware observer and its existing proof.
- `docs/configuration-source-policy.md`: inbound source-privacy operating delta only.
  Existing service HTTP/gRPC process proof remains with T2's complete delivery;
  no new test runner or production test-only interface.

Exclusive locks:
- Inbound observer source owners and configuration policy document;
  T2 starts after T1 writers stop and its changed baseline is integrated.

Final validation:
- Claim: S2's changed inbound paths withhold excluded values at their real local
  and exported boundaries while preserving admitted fields and response semantics.
- Checks: relevant tests selected/written by the executor; assembled final build,
  tests, documentation consistency and required independent review under
  [global Completion](../tasks.md). Preserve accepted proof scope from
  [Specification](../spec.md#proof-boundary-and-phase-handoff); exact cases,
  controls and commands remain Implementation-owned.
- Observable: no excluded inbound value in JSON/text or exported span names,
  attributes/events, including normalized method and server/client separation;
  current response semantics and correlation remain. These are local
  proof boundaries, not production/backend claims.

Reopen if:
Source privacy requires changing admitted request/RPC semantics or caller-visible
status/body: Specification. If role placement cannot preserve client identity:
System Design, then Rust Ownership. Routine implementation/test repair stays
with the Lead. Preserve unrelated work.
