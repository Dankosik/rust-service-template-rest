# T1 — Supported durable transfer admission

Outcome:
Startup rejects ACK-disabled/non-durable streams and an unestablishable or
undersized normal DLQ transfer path, preserving current runtime custody.

Consumes:
- [B1](../spec.md#b1-admit-an-ack-capable-usable-transfer-path) and [size mechanism](../design/system.md#stream-admission-and-supported-transfer-size-b1).
- [Ownership A](../design/ownership.md#responsibilities); prior #239 head `7223ea877f031d440842d3df6876857e91492ec2` in this repository, available from `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-messaging-durability-recovery-20261005`, supplies semantic storage/ACK-loss work only.

Provides:
- Role-aware file/default-persistence/ACK admission and registry-aware checked transfer bounds consumed by T3–T5.

Boundary:
Integrate equivalent #239 behavior onto current main while retaining #254 native
publication custody and #255 build policy. Enforce both total wire-size and
65,535-byte native header limits; no stream mutation, new grammar/configuration,
continuous drift enforcement or claimed production durability. Update the
existing durable-messaging explanation with the actual admission contract.

Mutable owners:
- `infra-messaging` admission/consumer/wire and their existing crate/provider tests.
- Existing `test/tests/messaging_outbox.rs` ACK-loss integration and narrowly related existing fixtures from #239.
- `docs/durable-messaging.md` admission/custody sections.

Exclusive locks:
- `infra-messaging` admission and provider-test owner; durable-messaging document.

Final validation:
- Claim: B1 refusal/transfer behavior and source custody hold with current Go wire and native lifecycle.
- Checks: Matching build/relevant tests plus existing real-broker/Go parity gates for changed surfaces; commands chosen during Implementation and consolidated at Completion.
- Observable: Unsafe role streams refuse before work; supported bounded transfers fit admitted limits; rejected/ambiguous transfer never settles away source custody. Historical #239 results are not new-candidate evidence.

Reopen if:
Provider sizing/persistence semantics invalidate T3 design or normal-envelope
behavior must change; route provider facts to Research, behavior to Specification,
mechanism to Technical Design. A mechanical merge conflict stays with this Lead.
