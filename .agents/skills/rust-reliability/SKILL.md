---
name: rust-reliability
description: "Budget arithmetic. Use for any Rust service decision about deadlines, timeouts, retries, overload, readiness, drain, shutdown budgets, or the consequence of a dependency failure."
---

# Rust Reliability

**Budget arithmetic.** Every wait, attempt, and teardown stage spends a parent budget that someone else already promised; trace the accepted budget through each hop to its terminal response or durable handoff. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Read the existing owners before adding policy. The config crate's HTTP section owns the request budget, the header timeout, the drain and propagation delay, the capacity bounds, and their validation, including that the request budget fits inside the drain left after propagation and that the connection cap is at least the in-flight limit. Bootstrap owns the grace deadline and the stage ceilings for diagnostics, background join, dependency close, and telemetry flush, and validates that the grace period covers the drain plus that tail. The health crate owns readiness cadence, failure threshold, and staleness. The hardened chain owns shedding with a retry hint and no queue, and the request timeout; the bounded server owns connection-level limits.

A new dependency call fits inside the request budget with a reserve for the terminal response: the attempts plus the backoffs plus the reserve must not exceed the parent. Use accepted values or values read from their owner; when a reserve, a backoff bound, or a useful-attempt minimum is not accepted, return that exact gap rather than choosing a number that makes the inequality fit. A retry also needs jitter and a safely repeatable effect; a queue needs a bound and a shed behavior; a degrade path needs a named signal. Proving that some attempt count does not fit says nothing about a smaller one; evaluate each proposal with the complete inequality.

A new pooled dependency contributes a probe, a readiness consequence, a close in the dependency stage of the shutdown plan under its budget, and a place in startup admission that decides whether traffic is admitted at all. Shutdown stages draw from the remaining grace deadline, so a slow stage shortens the ones after it instead of pushing the process into a kill.

For review, trace each affected failure path to its terminal disposition without editing, and reject any worst-case spend that exceeds the parent even when each attempt has its own timeout. For implementation, prove the bound with controlled coordination, such as a paused clock, an injected slow probe, or an occupied permit, and confirm the process test still observes readiness flipping before the listener closes. Do not add retries, circuit breakers, or a new budget knob without an accepted owner for its value.
