---
name: rust-debugging
description: "Causality. Use for an uncertain Rust defect when a failure, hang, incorrect result, startup rejection, or teardown overrun needs a discriminating explanation."
---

# Rust Debugging

Find the first observable divergence from the intended execution, then choose
an observation that can disprove the leading explanation. Separate what was
observed from what is inferred. An error at the end of a chain can be a consequence
of an earlier failure, and making it disappear does not establish a repair.

## Competing explanations

Use the symptom to narrow the search without declaring a cause. A compiler
failure calls for type or import evidence; a runtime failure needs the executed
path and its inputs. A hang may be a deadlock, starvation, external wait, missed
notification, or live work that exceeds its budget. Missing ownership and an
unbounded wait are candidates, not a universal diagnosis. Distinguish process
exit, caller timeout, cancellation request, and confirmed task completion.

Keep at least the materially plausible competing explanation until an observation
separates it. For a timeout, establish whether the operation was never admitted,
was waiting, was executing without progress, or completed without notification.
Use existing events, error causes, state transitions, or a bounded diagnostic
capture at that boundary. More logging everywhere can perturb scheduling and
bury the first divergence.

## A faithful reproducer

Reduce the reproducer to the smallest layer that preserves the failure. Retain
inputs, timing relationships, features, and dependency behavior that are causal;
a unit test that removes the contested synchronization no longer reproduces a
process race. Preserve stdout, stderr, exit status, and timing separately when
they discriminate explanations, while keeping secrets out of captures.

For an async flake, inspect scheduling and clock assumptions before raising
timeouts: sleep-based synchronization, a different clock under paused time,
notification registration order, detached work, or a dropped future with partial
progress can all matter. Make the relevant state transition observable and bound
the wait. For an ownership error, identify the required lifetime before adding
clones, shared ownership, or locking; these can move the symptom while changing
the intended custody.

Change one causal variable at a time where practical. If a proposed repair does
not change the discriminating observation, revisit the explanation rather than
stacking speculative fixes. Retries, swallowed errors, and caught panics can be
valid policy, but hiding the symptom is not evidence that they repair its cause.
Keep unrelated baseline failures distinct from the defect being diagnosed.

Use [Runtime Lifecycle](../../../docs/architecture/runtime-lifecycle.md) when
interpreting local startup/shutdown events, probe state, or exit meanings, and
[Configuration Source Policy](../../../docs/configuration-source-policy.md) when
selecting diagnostic settings. Diagnosis authorizes an explanation and the next
useful observation, not an implicit fix. Within authorized repair, change the
causal owner and replay the original failing condition with adequate regression
coverage; remove temporary instrumentation. Report residual uncertainty when
the evidence cannot distinguish the remaining explanations, through the current
execution owner rather than inventing another acceptance stage.
