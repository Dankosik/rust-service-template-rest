# T3 — Controlled recovery of one exact DLQ record

Outcome:
An operator can inspect, reconstruct, publish and deliberately retire one exact
record through an executable owned-session workflow with honest separate results.

Consumes:
- T1 Implemented admitted publication/custody behavior; [B4](../spec.md#b4-controlled-recovery-of-one-broker-dlq-record) and [Definition clarification](../definition-transition.md#b4-provider-driven-clarification).
- [DLQ mechanism](../design/system.md#one-record-dlq-workflow-b4), [fixture lifetime/resource design](../design/system.md#native-recovery-and-capacity-fixture-b2-b5), [ownership C/D/G](../design/ownership.md#responsibilities).

Provides:
- Thin `infra-messaging` DLQ example and persisted immutable selection/publication manifest.
- Owned R3/TLS session runner/Compose foundation reusable by T4/T5, including lifecycle custody, resource admission, bounded teardown and one-record native retirement.

Boundary:
Keep all layers of the usable B4 workflow together. Native delete has no CAS:
arbitrary remote inspection may work, but mutation refuses without original
owned broker lifetime custody. Runner owns exclusive topology and fresh
endpoint/credential generations after termination of an abandoned lifetime;
an expiring lease/readback/assumption flag cannot substitute. Manifest survives
ambiguity; only positive expected-stream PubAck permits exact-record retirement.
Include portable profile/make/classifier closure and operator docs with this
unit. R3 native restore/fault scenarios and measurements are later extensions.

Mutable owners:
- `crates/infra-messaging/examples/dlq_recovery.rs`, its example declaration and adjacent broker proof.
- `scripts/ci/messaging-recovery.sh` (optional same-owner Python helper), `test/fixtures/messaging-recovery-compose.yml`, session manifest/evidence format.
- Existing make, initializer/profile/classifier/self-test and selected CI entry owners for the new opt-in session capability; `docs/durable-messaging.md` operator sections.

Exclusive locks:
- Messaging example manifest/provider tests; messaging-recovery fixture/controller; profile/inventory/classifier/make/selected-CI contract; durable-messaging document.

Final validation:
- Claim: B4 stable identity and fence prevent stale or ambiguous operations from deleting another event, including crash/retry.
- Checks: Matching build/relevant tests, existing provider/wire/profile gates and owned-session demonstration required by B4; changed shell/workflow checks only for changed surfaces. One assembled run may supply multiple claims.
- Observable: Inspection classes, immutable redrive identity and publication/retirement states agree with actual exact-record state; unowned/stale generation refuses; lost responses preserve custody until resolved or lifetime ended.

Reopen if:
Owned process/credential/topology custody cannot fence delayed mutation: stop
only the dependent effect and route to platform/resource owner and Technical
Design. Missing native CAS does not authorize a weaker mechanism.
