---
name: rust-testing
description: "Observation. Use when constructing or reviewing Rust tests whose observing layer, oracle, dependencies, or coordination must distinguish a material behavior defect."
---

# Rust Testing

Start with the promise a defect would violate and the observation that would
expose it. Choose the smallest layer that includes the causal boundary: a unit
for a calculation, a mounted router for extraction and middleware, a connection
for server behavior, or a process for startup and exit. Small is useful because
failures are easier to explain, not because a lower layer can stand in for a
boundary it never crosses. Reuse coverage that already distinguishes the defect.

## An independent expectation

Derive expected results from the accepted contract, not the implementation's
current output. Literal boundary values often make the rule clear; independently
derived arithmetic, table loops, or a simpler reference oracle can be more
faithful for a broad input space. The question is whether the oracle would
repeat the same mistake. A production helper used to compute its own expected
result provides little protection even when the assertion looks detailed.

A round trip can prove internal agreement while an encoder and decoder share
the same wire defect. Add an independent expected representation or an independent
peer when interoperability is the claim. Preserve distinctions such as absence
versus invalid input, equality at a limit, a definitive failure versus an unknown
effect, and state before versus after an operation. Avoid incidental private
structure, call counts, or diagnostic formatting unless those are themselves
the promised behavior.

## Dependencies and coordination

Use a real dependency when its semantics are the subject of the claim: a double
cannot establish database commit behavior, and a directly invoked handler cannot
establish layer order. A protocol-speaking local peer may isolate client behavior
or a controlled fault more precisely than a complete deployment. A double can
exercise application policy around an unavailable provider, but must not
reimplement the behavior being asserted. Make that proof boundary explicit.

Synchronize on the event being tested: a notification, state transition, barrier,
or joined task. Sleeping long enough to usually win a race is not an ordering
oracle. Bound waits, retain ownership of spawned work, and join tasks or child
processes even on failure. For timer behavior, a controlled clock helps only
when the code uses that clock and the test lets the relevant task reach its
waiting state; advancing time alone does not establish scheduling progress.

For a defect, prefer a regression that fails for its causal reason rather than
an incidental timeout or setup error. Select property tests when an independently
stated invariant covers meaningful cases examples miss. Select mutation testing
when there is a concrete doubt about assertion sensitivity; a surviving mutant
can be equivalent or outside the claimed behavior, not necessarily missing
coverage. Neither technique is mandatory for every parser or every survivor.

For local harness choices and test ownership, consult
[Contributing](../../../CONTRIBUTING.md) and, when choosing helpers or a dependency
environment, [Backend Library Selection](../../../docs/backend-library-selection.md).
[Validation Routing](../../../docs/validation-routing.md) selects specialized
proof; [AGENTS](../../../AGENTS.md#validation-budget) and the active execution
owner determine when it runs. This method constructs the observation; it does
not add a completion gate.
