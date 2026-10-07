# T6 — Causal closure of region-span instability

Outcome:
The reported zero-versus-one region-span failure has a causal fix, or a current
already-landed repair is established with equivalent discriminating evidence.

Consumes:
- [B6](../spec.md#b6-trustworthy-feedback-during-development), [feedback design](../design/system.md#development-feedback-b6), [ownership E](../design/ownership.md#responsibilities).

Provides:
- Reliable existing telemetry assertion and causal diagnosis usable by final delivery.

Boundary:
Investigate current capture/dispatcher/callsite behavior before selecting a
repair. Preserve one operation span, Amazon region only and no-network presign.
Prefer existing supported tracing machinery if custom capture is causal. No
weaker assertions, ignored tests, unsupported cache-race claim, blanket
serialization or test-only production seam. Test-writing stays with the fix;
bounded discriminating coding feedback is not a per-task acceptance run.

Mutable owners:
- `crates/infra-object-storage/src/tests.rs`; `src/observe.rs` only if causal evidence identifies its existing instrumentation as defective.
- Compact causal findings under this task's research area; no other task's artifacts.

Exclusive locks:
- Object-storage tracing capture/instrumentation owner.

Final validation:
- Claim: B6's tracing defect is causally closed with the telemetry contract preserved.
- Checks: Matching build/relevant tests and accepted discriminating failed-before/passed-after evidence or equivalent proof of a landed repair; exact diagnostics and commands chosen by executor and retained for Completion.
- Observable: Captured enabled/callsite/new-span/record evidence distinguishes the cause, and the same meaningful assertions hold after repair. An unchanged passing rerun cannot close the defect.

Reopen if:
The required repair changes telemetry semantics: reopen Technical Design or
Specification. An unresolved cause remains owner-held investigation, not success.
