# T4 — Native recovery rehearsals with exact identity accounting

Outcome:
An adopter can execute the six B2 recovery situations using native snapshots
and real stores, obtaining exact recovered identities or the specified stop.

Consumes:
- T1 topology, T2 producer/effect modes, T3 owned session/fence; these are Implemented code dependencies.
- [B2](../spec.md#b2-execute-recovery-rehearsals-with-identity-evidence), [native fixture design](../design/system.md#native-recovery-and-capacity-fixture-b2-b5), [ownership D/G](../design/ownership.md#responsibilities).

Provides:
- Native coherent/mismatched restore, one-node loss, capacity exhaustion and retention-outage runner capabilities; per-ID durable-authority classification and evidence artifacts.

Boundary:
Extend T3's runner rather than creating a second fixture. Capture producer
business/outbox, source/DLQ and durable positions, receipts/effects; use native
NATS backup/restore and pg_dump/pg_restore. An oracle's bytes are not retained
recovery authority. Distinguish permitted mismatch/retention stops from failing
unexpected loss of acknowledged in-retention bytes. Include invocation,
interpretation, cleanup and existing manual/selected CI integration. No
production effects, invented RPO/RTO or host disk-full experiment.

Mutable owners:
- Messaging-recovery runner/Compose and scenario-specific integration proof beside the shared example, without changing its accepted effect logic.
- Existing make/classifier/profile/selected-CI owners only for scenario closure; `docs/durable-messaging.md` recovery guidance.

Exclusive locks:
- Messaging-recovery fixture/controller and integration recovery test file; profile/inventory/classifier/make/selected-CI contract; durable-messaging document.

Final validation:
- Claim: All six B2 situations have actual bounded observations; every fixture logical ID is accounted for at the selected boundary.
- Checks: Ordinary matching validation plus explicitly accepted B2 native runs on the assembled candidate; final delivery retains actual command/environment/identity receipts, selected by executors.
- Observable: Exact payload/ID and effect/receipt state establish recovery or a permitted named stop. Skipped setup/scenario is unverified; unexpected lost retained ACKed data fails. Capture actual restore/catch-up durations without adopting SLOs.

Reopen if:
Native archives/provider behavior invalidate the mechanism, or authorized
resources cannot meet the 1 GiB data/3 GiB container RAM/2 GiB free-disk/15-minute
run bounds. Preserve code progress; unavailable required observation blocks
Completion, not unrelated implementation. Build feasibility is separate.
