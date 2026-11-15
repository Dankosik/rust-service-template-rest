---
name: rust-performance
description: "Evidence. Use to assess or measure Rust latency, throughput, allocation, memory, or capacity claims, or profile and allocator changes."
---

# Rust Performance

**Evidence.** Identify the requested result (audit, measurement, or optimization), and define the workload, constraints, metric, and comparable baseline before choosing a change.

Latency percentiles, throughput, allocation rate, retained memory, peak resident memory, startup time, and binary size are different claims. Measure the artifact and configuration that represent accepted use (normally a release build, not debug execution or `cargo run` timing), keeping toolchain, features, inputs, and environment comparable across samples and retaining variance. An audit without runtime evidence can establish code properties and propose measurements, not declare a bottleneck or speedup.

Choose algorithms and data structures against actual input sizes and operations; a theoretically stronger algorithm may cost more for small inputs. Avoid unnecessary copying, temporary collections, and repeated work only when the complexity trade-off is justified, preserving streaming, ordering, and failure timing. A clone at startup can still matter for startup latency or retained memory; prioritize it by the requested outcome rather than its location alone.

Measure layout when frequently instantiated types make memory or copying material, accounting for large enum variants, alignment, padding, and indirection costs. Do not contort an API for an unmeasured saving. An optimization that moves cost into an unbounded queue, cache, buffer, or detached execution is a regression in resource control even if the foreground call becomes faster.

Choose evidence that separates plausible causes (repeated command samples for startup/shutdown, a realistic microbenchmark for one operation, a CPU profile for computation, allocation/retention evidence for memory) and account for instrumentation overhead. Consult existing runtime telemetry and the HTTP resource-lifetime owner in `crates/infra-http` before treating handler limits as a bound on all retained response work.

Treat allocator, LTO, codegen-unit, panic-strategy, and PGO changes as alternatives with named trade-offs; a service using unwind-based panic isolation cannot silently switch to abort. Follow the [build-speed owner](../../../docs/build-speed.md) when applicable; neither a full-repository benchmark suite nor a new runner follows from mentioning a tool.

When evidence is unavailable or inconclusive, report the limit and retain the simpler correct choice without inventing an improvement.
