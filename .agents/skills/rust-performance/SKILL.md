---
name: rust-performance
description: "Evidence. Use to assess, measure, or verify Rust latency, throughput, allocation, memory, or capacity claims, or a proposed allocator, profile, or optimization setting."
---

# Rust Performance

**Evidence.** Identify the requested result: an audit, a measurement, or an optimization. Define the relevant workload, constraints, and metric before choosing a change; establish a comparable baseline for a measured claim. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Latency percentiles, throughput, allocation rate, retained memory, peak resident memory, startup time, and binary size are different claims. Measure the artifact and configuration that represent the accepted use; for runtime optimization this normally means a release build, not debug execution or cargo run timing. Keep toolchain, features, inputs, and environment comparable across samples, and retain variation instead of rerunning until a favorable sample appears. An audit without runtime evidence can establish code properties and propose measurements, not declare a bottleneck or speedup.

Choose algorithms and data structures against actual input sizes and operations. A theoretically stronger algorithm may cost more for small inputs; neither small nor large input is an assumption supplied by this skill. Avoid unnecessary copying, temporary collections, and repeated work, but compare their costs with the complexity introduced by removing them. Useful size information may justify capacity reservation; collecting an iterator may already use it. Preserve streaming, ordering, and failure timing. A clone at startup can still matter for startup latency or retained memory; prioritize it by the requested outcome rather than its location alone.

Measure layout when frequently instantiated types make memory or copying material. A large enum variant can increase every instance's size; representation, discriminants, alignment, and padding also matter. Indirection trades layout for allocation and access cost and needs evidence. Borrowed views and owned values each have useful lifetimes; do not contort an API for an unmeasured saving. An optimization that moves cost into an unbounded queue, cache, buffer, or detached execution is a regression in resource control even if the foreground call becomes faster.

Choose evidence that separates plausible causes: repeated command samples for startup or shutdown, a realistic microbenchmark for one operation, a CPU profile for computation, and allocation or retention evidence for memory. Account for instrumentation overhead. In this service, consult existing runtime telemetry and the [HTTP resource-lifetime policy](../../../docs/architecture/http.md#request-and-response-resource-lifetime) before treating handler limits as a bound on all retained response work. Use existing accepted workload and capacity owners rather than inventing new limits.

Treat allocator, link-time optimization, codegen-unit, panic-strategy, and profile-guided changes as alternatives with named trade-offs. A service using unwind-based panic isolation cannot silently switch to abort. Follow the project's [build and validation policy](../../../AGENTS.md#validation-budget) and [build-speed owner](../../../docs/build-speed.md) when applicable; neither a full-repository benchmark suite nor a new runner follows from mentioning a tool.

For analysis, report supported properties separately from performance hypotheses without editing. For benchmarking, report the comparison without requiring a code change. For an authorized optimization, preserve behavior, error identity, ordering, and resource bounds alongside the measured result. When evidence is unavailable or inconclusive, report the limit and retain the simpler correct choice; do not invent an improvement.
