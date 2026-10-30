---
name: rust-errors
description: "Outcome meaning. Use when Rust failures, absence, partial success, uncertain effects, degradation, or cleanup need distinctions that determine what a caller can safely do next."
---

# Rust Errors

Classify the outcome where the operation still knows what happened, then carry
the distinctions its consumers need. Success, absent input, invalid present
input, partial success and an unknown effect permit different next actions.
A failed observation does not prove the attempted effect failed: after a lost
acknowledgement, blind retry can repeat work that already succeeded. Retain that
uncertainty until an authoritative observation or the operation's recovery
policy resolves it.

## Preserve distinctions that change action

For an optional value, absent input may be `Ok(None)` while malformed present
input remains `Err`; `parse().ok()` collapses a caller error into absence. When
the input is required, absence is an error too, potentially with a distinct
diagnostic. Load [outcome examples](references/outcome-examples.md) when choosing
between these policies or translating an optional parser result; the examples
preserve invalid input under both policies. A default is appropriate only for
the states the accepted contract treats as equivalent.

Use typed identity when callers branch on recovery, rejection, fallback or
reconciliation. Match that identity rather than display text. An opaque error
can be adequate within a boundary whose consumers perform the same action for
every failure; it need not be restricted to a composition root. Conversely,
one enum per module adds no value when it only mirrors another owner's variants.
Preserve the causal error through a source or equivalent structured context
when translating, while exposing only the distinctions the new boundary owns.
Avoid leaking provider internals into a stable public contract merely to retain
diagnostic detail. Context should identify the failed operation or safe resource
identity without formatting secret-bearing input or nested payloads.

For partial success, expose which effects are confirmed and which remain
failed or unknown if callers need that information to continue safely. A single
`Err` that hides completed work invites unsafe replay; a single `Ok` that hides
required failed work invites false completion. Degradation is legitimate when
the accepted operation can still meet its reduced promise and that state is
observable. Logging a failure alone does not make a success result truthful.

## Separate completion from destruction

When flush, close or commit can fail or wait, provide an explicit completion
operation and an owner that observes its result within the applicable budget.
`Drop` can release infallible resources or request best-effort cleanup without
panicking or blocking; it cannot report durable completion. Keep simple resource
release in `Drop` when there is no separate outcome to observe. If cleanup also
fails after the main operation failed, retain both and preserve the primary
cause. If the main effect succeeded, a cleanup failure cannot undo it: reporting
generic failure without the confirmed effect may trigger a duplicate retry.

At an externally visible mapping, load [HTTP Architecture](../../../docs/architecture/http.md)
for Problems and contract policy, and [Runtime Lifecycle](../../../docs/architecture/runtime-lifecycle.md)
for startup, degradation, teardown and exit meaning. Their local mappings decide
which outcome is reported; this method does not invent status codes or an exit
policy. Panic containment likewise needs an observer: catching a request panic
does not observe a spawned task's failure or prove its effects were rolled back.
