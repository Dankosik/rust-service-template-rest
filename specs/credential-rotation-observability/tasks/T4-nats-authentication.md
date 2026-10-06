# T4 — Real NATS authentication in the existing integration route

Outcome:
Make the existing messaging route execute the R3 credential replacement,
old/expired refusal and valid-file recovery outcomes against an authenticated
NATS broker, with truthful scope and lifecycle results.

Consumes:
- T1 implemented challenge telemetry and released messaging guide ownership.
- [Specification R3](../spec.md#r3-real-authentication-during-nats-and-valkey-file-rotation).
- [Design NATS fixture](../design/design.md#nats-a-serialized-authenticated-segment-of-the-existing-broker-harness)
  and [ownership](../design/design.md#ownership-and-dependency-decision) —
  closed synthetic signing/trust, target, lifecycle/CI and removal decisions.
- [Design evidence](../design/evidence.md) — resolved producer/reference and
  pinned server facts; evidence is not authority over external servers.

Provides:
- The authenticated `credential_rotation` target, synthetic trust assets and
  runner/CI/profile-removal wiring as one consumable regression outcome.

Boundary:
Keep target, synthetic assets, harness admission/restoration, compile-only
selection and CI routing together. Run ordinary suites before the authenticated
segment on the same pinned disposable service; no concurrent broker consumer
survives the switch. Adopt the exact Design managed/unmanaged input contracts,
published-endpoint identity, cleanup and failure propagation. Unmanaged supplied
endpoints are never reconfigured. No anonymous fallback may explain success.
Fixtures use existing signing/JSON/base64 dependencies and test-local glue;
production runtime remains unchanged apart from T1. No general issuer, new
server/service/job/runner, normal-config mutation or dependency upgrade.

Mutable owners:
- `crates/infra-messaging/tests/credential_rotation.rs`, its test fixture assets,
  the crate manifest's target registration and reusable test-local relay support
  only where required by this authenticated target.
- `env/nats/credential-rotation.conf`: explicitly synthetic trust configuration.
- `scripts/ci/test-integration-messaging.sh`: sole service-lifecycle owner.
- `.github/workflows/ci.yml`: explicit identity/exclusive delegation for the
  existing integration project and messaging invocation, retaining sequence.
- `scripts/lib/template_profiles.json`: new messaging-only asset removal.
- `docs/durable-messaging.md` and the messaging subsection of
  `docs/build-test-and-development-commands.md`: authenticated invocation,
  ownership, compile-only and proof scope, preserving T1 signal text.
- Existing secret-scan allowlist only if the current scanner flags the public
  synthetic assets: exact fixture paths only, never a disabled/general rule.

Exclusive locks:
- Messaging integration target/fixture assets and crate manifest.
- Messaging runner, CI workflow, profile-removal registry, messaging/command
  guides, and the conditional exact-path secret-scan allowlist.
- At actual execution, exclusive lifecycle ownership of the selected disposable
  Compose project's `nats` service. The CI database/outbox and anonymous broker
  consumers finish before the authenticated segment; restoration finishes before
  another caller consumes the normal broker. No runtime service is started by
  Planning or ordinary test authoring.

Final validation:
- Claim: NATS R3 actually executes on the broker's JWT/seed authentication
  boundary; accepted preparation alone cannot stand in for broker acceptance.
  Runner scope, cleanup and CI/profile selection preserve the existing suite.
- Checks: Accepted R3 requires a matching real run through the existing local/CI
  path. Existing `messaging_integration` gate must execute the authenticated
  cases on the assembled candidate, including old/expired refusal and recovery.
  Current delivery/security/profile checks apply to changed scripts/workflow,
  manifest and synthetic material under their existing owners. Cases, assertions
  and commands remain executor choices for the single final delivery stage.
- Observable: Authenticated operation/reconnect, refusal and recovery evidence
  excludes disabled authentication or stale captured file material; failure of
  the authenticated segment or restoration fails the harness. Missing, filtered
  away, skipped or compile-only scenarios never establish full R3 success.

Reopen if:
Design owns infeasible trust/fixture/lifecycle isolation or materially changed
flow; Research owns contradictory pinned contracts; Specification owns changed
acceptance semantics. Harness/test repair within this outcome remains here.
