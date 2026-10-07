# T5 — Measured capacity and shared-role failure envelope

Outcome:
The real R3/TLS producer/outbox/consumer/effect path yields attributable bounded
capacity and outage/catch-up evidence for the three accepted operating choices.

Consumes:
- T1 topology, T2 shared-pool example, T3 owned R3/TLS session; Implemented code, not passing receipts. T4 is not a semantic prerequisite; serialize its shared writers through locks.
- [B5](../spec.md#b5-measured-capacity-and-failure-domain-decisions), [measurement/reopen design](../design/system.md#native-recovery-and-capacity-fixture-b2-b5), [ownership D/G](../design/ownership.md#responsibilities).

Provides:
- Repeatable bounded measurements with ordinary jobs present, time-series and exact-ID accounting, plus the evidence format for final retain/change dispositions.

Boundary:
Extend the same runner with current one-publisher/shared-role baseline,
1 KiB events and bounded 64 KiB slice, stable-to-growing backlog, outage and
catch-up. Collect the design's offered/admitted/confirmed/applied, queue age,
pool/query/database/broker and effective consumer-limit indicators. Keep R3,
TLS and host limitations visible. Include invocation/report interpretation
and portable selected execution. Initially retain concurrency 1, effective
broker-default MaxAckPending and shared roles. A runtime change first requires
attributable evidence and the corresponding reviewed Technical Design delta;
it is then a scoped implementation repair, never a blind tuning task.

Mutable owners:
- Messaging-recovery runner/Compose measurement modes and ordinary-job probe in the example entry; accepted effect logic stays unchanged.
- Existing make/classifier/profile/selected-CI owners for measurement closure; `docs/durable-messaging.md` capacity interpretation.
- Actual result report/JSON/CSV locators under this task's research area are produced by the final delivery owner, not invented during implementation.

Exclusive locks:
- Messaging-recovery fixture/controller and example entry; profile/inventory/classifier/make/selected-CI contract; durable-messaging document.

Final validation:
- Claim: B5 actual representative results justify all three final retain/change dispositions and describe ordinary-job interaction.
- Checks: Ordinary matching checks plus the explicitly accepted bounded R3/TLS measurement on the assembled candidate, sharing release artifacts with B2. Repeated command comparisons, if selected, use `hyperfine --warmup 3` under the existing instruction.
- Observable: Time series and exact IDs distinguish backlog growth, saturation, outage recovery and collateral role failure. Headroom/consumer-slot/ordinary-job evidence triggers only its named reopen; no gain is claimed from configuration alone. Missing required observations stay incomplete.

Reopen if:
Attributable evidence meets one of the three design triggers, or resource limits
cannot support the representative observation. Route the narrow runtime choice
to Technical Design before code changes; final owner reruns only affected proof.
