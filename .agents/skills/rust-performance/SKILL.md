---
name: rust-performance
description: "Evidence. Use to assess or measure Rust latency, throughput, allocation, memory, or capacity claims, or profile and allocator changes."
---

# Rust Performance

**Evidence.** Identify the requested deliverable—static audit, measurement, or optimization—and fix the workload, resource bounds, target metric, and comparable baseline before changing code.

**Comparable measurement.** Separate tail latency, throughput, allocation rate, retained memory, peak RSS, startup time, and binary size. Measure release-profile artifacts under matched toolchain, features, inputs, and host conditions while reporting variance; present static code audits as candidate hypotheses and measurement plans until runtime numbers exist.

**Data structures and work.** Match algorithms and representations to actual input distributions, removing copies, intermediate collections, or repeated passes where measured gains outweigh added complexity and preserving streaming, ordering, and failure semantics. Prioritize startup or hot-path clones according to the metric that matters.

**Layout and resource bounds.** Inspect size, enum variant skew, alignment, and indirection on high-volume types while keeping APIs clean until measurements justify layout changes. Keep queues, caches, buffers, and background tasks bounded so foreground latency wins never trade away memory or backpressure control.

**Profiling and build trade-offs.** Pick the probe that isolates the bottleneck—repeated process runs for startup/drain, focused microbenchmarks for pure operations, CPU profiles for compute, and heap profiles for retention—accounting for profiler overhead and `crates/infra-http` post-handler response lifetimes. Weigh allocator, LTO, codegen-unit, panic-strategy (preserving unwind isolation), and build-speed trade-offs via the [build-speed owner](../../../docs/build-speed.md), retaining the simpler correct implementation whenever measurements stay inconclusive.
