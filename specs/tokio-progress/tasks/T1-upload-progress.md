# T1 — Cooperative exact-length upload progress

Outcome:
Replace unbounded inner polling of immediately-ready empty frames with the accepted 64-inner-poll quantum in `ExactLength`, while preserving the existing exact-length stream result and wakeup contract.

Consumes:
- [Specification: Upload progress](../spec.md#upload-progress) — accepted behavior and compatibility.
- [Mechanism: Upload polling](../design/mechanism.md#upload-polling) — numeric quantum, state retention and self-wakeup decision.
- [Ownership map](../design/ownership.md#files) — existing private adapter and test placement.

Provides:
- The bounded adapter and its required implementation-owned coverage, usable independently of logging changes.

Boundary:
Modify the existing private upload adapter and associated tests. Each wrapper poll spends at most 64 inner polls, including end confirmation; a budget yield preserves held data/state and arranges another poll. Preserve source Pending/error behavior, underflow/overflow, trailers, size hints and end-of-stream semantics. No scheduler abstraction, Tokio-only assumption, dependency or public error change.

Mutable owners:
- `infra-object-storage` body adapter in `crates/infra-object-storage/src/body.rs` and existing upload/body test owners for this delta.

Exclusive locks:
- none.

Final validation:
- Claim: Ready empty frames cannot monopolize one wrapper poll, and valid finite streams and invalid-length streams retain their accepted semantics.
- Checks: Repository local completion and relevant tests selected/written by Implementation; final assembled concurrency review includes this unit. No additional environment or performance threshold.
- Observable: Finite bounded wrapper work with a live wakeup path and unchanged upload integrity at the selected local proof boundary. This does not establish progress inside an arbitrary blocking source poll or a latency SLA.

Reopen if:
Technical Design if a quantum cannot preserve adapter state/length/trailer semantics under the selected mechanism; Specification only if accepted behavior must change. Routine test choices and local implementation repair remain with Implementation.
