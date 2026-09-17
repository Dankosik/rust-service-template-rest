---
name: rust-performance
description: "Evidence. Use to assess, measure, or verify a Rust service latency, throughput, allocation, memory, or capacity claim, or a proposed allocator, profile, or optimization setting."
---

# Rust Performance

**Evidence.** Identify the requested result: an audit, a measurement, or an optimization. Define the workload and the metric, and establish a comparable baseline before changing anything. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Separate the properties, because they are different claims with different measurements: request latency percentiles, throughput under the in-flight limit, allocation rate, live heap and peak resident memory, startup time, and binary size. Measure a release build directly; a cargo run or a debug profile is not a comparable baseline. Keep toolchain, features, input, and environment fixed across samples, and retain the variance rather than rerunning until a favorable sample appears.

In request paths, keep data moving: pass byte buffers and borrowed slices, stream bodies when the contract allows, and account for what each retained buffer, queue, and cached verdict holds as concurrency grows. The body limit, the in-flight limit, and the request timeout are the capacity model; an optimization that moves cost into an unbounded place is a regression. A clone on a hot path matters and a clone at startup does not.

Treat link-time optimization, codegen units, the panic strategy, allocator changes, and profile-guided optimization as measured alternatives with named trade-offs. Aborting on panic disables the sanitized server error that panic recovery provides, and a custom allocator is a container-memory decision rather than a default. The release profile changes only with a measurement attached to the change.

Choose evidence that separates plausible causes: repeated whole-command samples with hyperfine for startup or shutdown time, a microbenchmark for one operation with realistic inputs, a CPU profile for computation, an allocation profile for churn. Account for profiler overhead, and read the Tokio runtime metrics already exported before adding instrumentation.

For an audit without runtime evidence, report code-supported properties separately from bottleneck hypotheses and propose the discriminating measurement without editing. For benchmarking, report the comparison without requiring a code change. For an authorized optimization, verify behavior, error identity, ordering, and resource bounds alongside speed, and report before-and-after evidence with its limits. When evidence is unavailable or inconclusive, say so and prefer the simpler correct implementation rather than inventing a speedup.
