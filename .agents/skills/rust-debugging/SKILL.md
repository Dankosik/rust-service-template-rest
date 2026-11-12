---
name: rust-debugging
description: "Causality. Use for an uncertain Rust defect: a failing or flaky test, a hang, a wrong response, a startup rejection, or teardown that overran its budget."
---

# Rust Debugging

**Causality.** Find the first observable divergence between intended and actual behavior, and choose an observation that could disprove the leading explanation. Keep the symptom, evidence, hypothesis, and proposed repair distinct. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Classify the failure before selecting an environment. Compiler and lint diagnostics need no process; a decoding rejection, runtime panic, deadlock, missing wakeup, exhausted resource, or uncertain external effect needs evidence from its own boundary. A task owner or timeout can exist while runtime progress is stalled, so do not infer the cause of a hang merely from their presence or absence. Inspect structured errors and their causes before interpreting a top-level message.

Reproduce at the smallest available layer that retains the same failure: a pure operation, controlled async test, mounted router, bounded local peer, or process as appropriate. Preserve output, exit status, event order, and timing when those distinguish explanations. Use the diagnostic facilities the selected executable actually exposes. In this service, `crates/infra-telemetry` and the composition root in `crates/service/src/bootstrap` own startup and shutdown signals; the installed panic hook withholds payloads and backtraces even when RUST_BACKTRACE is set. Do not bypass that disclosure policy to obtain a convenient trace.

For uncertain async behavior, inspect admission, ownership, notification registration, cancellation points, lock scope, and executor progress. Common candidates include sleep-based synchronization, incompatible clocks under paused time, a detached task, a dropped future with meaningful partial progress, or blocking work on an executor thread. These are hypotheses to discriminate, not a checklist of defects presumed to exist. For an ownership diagnostic, recover the actual lifetime and data flow before adding a clone, Arc, or lock.

Minimize the reproducer only while it retains the relevant cause and boundary. Change one causal variable at a time and compare the observation with the prediction. After an ineffective fix, revisit the explanation instead of stacking compensating branches, retries, or timing increases. A swallowed error, caught panic, or longer wait can conceal a mechanism without repairing it. Keep unrelated baseline failures separate, and keep diagnostic captures free of secrets.

For diagnosis, finish with a supported cause or the next discriminating observation and its missing input; do not edit when fixing is outside scope. For an authorized fix, change the causal owner, replay the original failure, and retain focused regression coverage that detects the defect rather than its current implementation. Reuse an adequate existing reproducer, remove task-owned temporary instrumentation, and report actual verification. Do not expand into unrelated cleanup or build a new environment merely because an optional diagnostic is unavailable.
