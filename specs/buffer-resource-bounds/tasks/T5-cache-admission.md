# T5 — Bound cancellable cache work

Outcome:
Redis application work has immediate finite admission and cannot accumulate unanswered commands by cancelling callers, while maintenance and ordinary recovery remain usable.

Consumes:
- [S4](../spec.md#s4-bound-redis-outstanding-work-across-caller-cancellation), [selected retirement design](../design/selected-design.md#s4-immediate-cache-admission-and-generation-retirement), [R5/R9 ownership](../design/ownership.md#files).
- [S6](../spec.md#s6-correct-and-complete-the-adopter-facing-explanation) cache key/value/decoded allocation and server-policy guidance.

Provides:
- Resource-level 256 immediate application slots, one separate external-probe slot, and synchronous possible-dispatch generation retirement before capacity reuse; cache guidance states its actual limits.

Boundary:
Use Link and the existing supervisor only. Preserve command deadline, generation identity, credentials/redaction, last-owner cleanup, recovery and no write replay. Keep separately finite supervisor maintenance, native pipeline 50 and 8 KiB flush threshold. No key/value ceiling, queue, spawned driver or readiness redesign.

Mutable owners:
- infra-cache connection lifecycle: crates/infra-cache/src/connection.rs, adjacent tests and crates/infra-cache/src/tests.rs; existing crates/infra-cache/tests/valkey.rs only if its accepted contract requires adjustment.
- docs/cache.md: command admission/cancellation plus feature-owned bytes/decoded results and server maxmemory/eviction.
- docs/cache-decisions.md: one-line consistency correction for the existing fan-in statement; preserve absence of a byte or RSS ceiling.

Exclusive locks:
- Cache connection lifecycle and existing cache fixtures; no other planned unit mutates them.

Final validation:
- Claim: cancellation after possible dispatch retires only its generation before permit release; admission refusal occurs before dispatch, and maintenance/recovery/no-replay invariants hold.
- Checks: ordinary local criterion once at assembled Completion with existing fixtures and executor-selected cases/commands. Existing optional provider integration stays CI-owned; no new server provisioning.
- Observable: bounded live command ownership and stable sanitized outcomes/recovery at the exercised boundary; count is not a value or RSS cap.

Reopen if:
Retirement cannot preserve the accepted lifecycle or budgets: Technical Design; changed failure/no-replay policy: Definition.
