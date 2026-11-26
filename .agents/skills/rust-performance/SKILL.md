---
name: rust-performance
description: "Evidence. Use to assess or measure Rust latency, throughput, allocation, memory, or capacity claims, or profile and allocator changes."
---

# Rust Performance

**Evidence.** Identify the requested deliverable—static audit, measurement, or optimization—and fix the workload, resource bounds, target metric, and comparable baseline before changing code.

**Comparable measurement.** Separate tail latency, throughput, allocation rate, retained memory, peak RSS, startup time, and binary size. Measure release-profile artifacts under matched toolchain, features, inputs, and host conditions while reporting variance; present static code audits as candidate hypotheses and measurement plans until runtime numbers exist.

**Data structures and work.** Match algorithms and representations to actual input distributions, eliminating quadratic work (`Vec::remove(0)` → `into_iter()` or `VecDeque`), repeated map lookups (`Entry` API), redundant buffer growth (`clone_from`, `with_capacity`), unused formatting on cold branches, and intermediate collections (`fmt::from_fn`, `chunks_exact`, `windows`, `chunk_by`); see [hot-path idioms](references/hot-path-idioms.md). Flush and shut down buffered writers (`tokio::io::BufWriter` / `std::io::BufWriter`) explicitly so drops never hide write errors, and delegate hot generic entry points (`impl AsRef<Path>`, `impl Into<T>`) immediately to a non-generic inner function to prevent monomorphization bloat.

**Layout and resource bounds.** Inspect size, enum variant skew, alignment, and indirection on high-volume types while keeping APIs clean until measurements or clear skew justify changes: box rare large `enum` or `Result::Err` variants and large sub-futures (`Box::pin`), and store long-lived immutable collections as `Box<[T]>`, `Box<str>`, or `Arc<str>`. Keep queues, caches, buffers, and background tasks explicitly bounded so foreground latency wins never trade away memory or backpressure control.

**Profiling and build trade-offs.** Pick the probe that isolates the bottleneck—repeated process runs for startup/drain, focused microbenchmarks for pure operations, CPU profiles for compute, and heap profiles for retention—accounting for profiler overhead and `crates/infra-http` post-handler response lifetimes. Weigh allocator, LTO, codegen-unit, panic-strategy (preserving unwind isolation), and build-speed trade-offs via the [build-speed owner](../../../docs/build-speed.md), retaining the simpler correct implementation whenever measurements stay inconclusive.
