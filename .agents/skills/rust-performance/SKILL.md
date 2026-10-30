---
name: rust-performance
description: "Measurement. Use when a Rust latency, throughput, allocation, memory, or capacity claim needs a comparable workload, causal bottleneck evidence, or an optimization tradeoff."
---

# Rust Performance

Define the workload and metric before selecting an optimization. An audit can
identify plausible costs; a measurement establishes their size under stated
conditions; an optimization needs a comparable baseline and candidate. Keep
these claims distinct so a code-level hypothesis does not become an asserted
speedup.

## Comparable observations

Choose the observation that matches the requested outcome: latency distribution,
throughput at a stated concurrency and error rate, allocation rate, retained
heap, peak resident memory, startup cost, or artifact size. These can move in
opposite directions. A faster median does not establish a better tail, and fewer
allocations need not reduce retained memory. Include failures, saturation, and
workload scale when they affect the result.

Compare the same input distribution, build profile, features, toolchain,
dependencies, and environment. For a production performance claim use the
representative optimized binary; measuring compilation or debug execution
answers a different question. Record warmup, sample variation, and material
interference instead of rerunning until a favorable sample appears. A small
change within noise is inconclusive. Follow the current command-comparison
policy for requested command benchmarks, including `hyperfine --warmup 3`.

## Cost and its owner

Use measurements to locate the limiting resource before moving work. A CPU
profile distinguishes computation from waiting; allocation evidence distinguishes
churn from retention; whole-command samples include startup and teardown that a
microbenchmark omits. Instrumentation has overhead, so compare like with like
and use existing observations before introducing more. A microbenchmark matters
only to the larger outcome in proportion to that operation's workload share.

Account for bytes and lifetimes as concurrency grows: per-request buffers,
queued inputs, active operations, caches, and residual work after timeouts can
all retain memory simultaneously. Streaming can reduce peak buffering but
extend connection and permit custody. Cloning can simplify ownership and be
cheap; assess its frequency, input size, and retention. A large startup clone
can affect peak memory or repeated initialization, while a tiny frequent clone
may still be irrelevant to the measured bottleneck.

Treat an allocator, LTO, codegen-unit, panic-strategy, or profile change as an
alternative with build, memory, latency, and behavioral costs. For local
release-profile measurement and unwind/recovery constraints use
[AGENTS](../../../AGENTS.md#rust-change-surface);
for the selected allocator and its evidence use
[Backend Library Selection](../../../docs/backend-library-selection.md#global-allocator).
The current profile in [Cargo.toml](../../../Cargo.toml) is the actual build
input; a generic optimization preference does not override its accepted choices.

When optimization is authorized, preserve observable behavior, failure identity,
ordering, and resource bounds alongside the metric. A faster path that drops
work or moves retention into an unbounded queue is a different result. Report
the measured difference and its limits, or the missing discriminating measurement
when evidence is inconclusive. Keep the simpler correct implementation when the
proposed complexity has no supported benefit; a benchmark need not produce a
code change.
