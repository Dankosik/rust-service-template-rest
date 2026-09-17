---
name: rust-performance
description: "Use when a change claims or needs a latency, throughput, allocation, memory, or capacity property, or proposes an allocator, profile, or optimization setting."
metadata:
  invocation: model
  kind: method
---

# Rust Performance

Performance work is **evidence**: name the workload and metric, establish a
comparable baseline, then change one causal variable.

Separate the properties: request latency percentiles, throughput under the
in-flight limit, allocation rate, live heap and peak resident memory, startup
time, and binary size are different claims with different measurements.
Measure a release build directly; `cargo run` and debug profiles are not
comparable baselines. Keep toolchain, features, input, and environment fixed
across samples; keep the variance, do not rerun until a favorable sample
appears.

In request paths, keep data moving: pass `Bytes` and borrowed slices, stream
bodies instead of buffering when the contract allows, and account for what
each retained buffer, queue, and cached verdict holds as concurrency grows.
Bounded work per request (`max_body_bytes`, `max_in_flight`, the request
timeout) is the capacity model; an optimization that moves cost into an
unbounded place is a regression. A `clone` on a hot path matters; a `clone`
at startup does not.

Treat LTO, codegen units, `panic = "abort"`, allocator changes (`jemalloc`,
`mimalloc`), and PGO as measured alternatives with named trade-offs:
`panic = "abort"` disables the sanitized-500 panic recovery, and a custom
allocator is a container-memory decision, not a default. The release profile
in `Cargo.toml` changes only with a measurement attached to the change.

Prefer existing tools: `hyperfine` for whole-command comparisons, `criterion`
for a specific operation once the benchmarking stage lands, a CPU profile for
computation, an allocation profile for churn. Account for profiler overhead
and for the Tokio runtime metrics already exported on `/metrics`.

For an audit without runtime evidence, report code-supported properties
separately from hypotheses and propose the discriminating measurement without
editing. For an authorized optimization, verify behavior, error identity,
ordering, and bounds alongside speed, and report before/after numbers with
their limits. When evidence is unavailable, say so and keep the simpler
correct implementation.
