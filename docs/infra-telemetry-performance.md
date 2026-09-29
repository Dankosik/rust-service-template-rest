# Telemetry allocation improvements

The changes in `infra-telemetry` retain three independently measured mechanisms:
direct JSON correlation-field serialization, resource attributes supplied without
a temporary growable vector, and copying the bounded UTF-8 error prefix once.
There are no dependency, filtering, sampling, metric, exporter or lifecycle changes.

## DigitalOcean evidence

The 2026-09-28 comparison used baseline
`9631b0020e9efbf5df5026e005d898d083ce0db5`, pinned Rust 1.98.1 and locked
dependencies on one DigitalOcean c-4 in `lon1`: four virtual CPUs, 8 GiB RAM,
Intel Xeon Platinum 8280 under KVM. The integration base `54907fc` has identical
telemetry source, workspace manifest/lockfile and toolchain inputs.

Each isolated candidate/workload cell used three warmups, six baseline/null
pairs and twelve alternating baseline/candidate pairs. Loop counts came only
from baseline calibration. Allocation measurements used three pairs of N/2N
heaptrack profiles; GNU time measured uninstrumented process RSS separately.
All samples were retained. The speed reporting floor was max(5%, twice median
null-pair noise); this heuristic is not a statistical confidence interval.

| Retained mechanism | Observed result | Limit |
| --- | --- | --- |
| Direct correlation-field writer | Established-span JSON event: 6.00% median paired elapsed-time reduction; median five allocation calls and 693 requested bytes saved per event | Explicit-parent, nested and unsampled controls improved 5.01–6.35%; no-provider/no-span controls were neutral. The request-like fixture's 4.37% time reduction was below the floor. |
| Resource attribute iterator | With instance ID: two allocation calls and 678 requested bytes saved per construction in all three pairs; without ID: approximately one call and 196–246 bytes saved | Startup-only. The 2.84%/0.75% elapsed-time changes were below the speed floor. |
| Bounded error-prefix copy | Long ASCII: 56.82% less time, approximately 327→141 ns; short error: 75.25%, 133→33 ns; Unicode: 71.92%, 848→237 ns | Exporter-construction failure only. Short/Unicode cases dropped from three allocations to one; requested bytes changed 56→21 and 1,400→468 respectively. |

These are isolated operation measurements. Logging used synchronous `/dev/null`
output with real JSON formatting and trace contexts, but no exporter endpoint.
They establish neither whole-service throughput nor collector delivery, tail
latency, slow-sink performance or additive gains from the assembled patch.
Peak heap/RSS did not materially improve; the memory benefit is allocation churn.

The broader review covered six hypotheses and 22 cells. Removing the boxed
format layer and rendering scrapes directly to bytes did not improve speed.
A static-level filtering fast path improved isolated span enter/exit work, but
its request-like controls stayed below the floor; that extra configuration branch
is not retained in this delivery.

## Behavior and custody

Before integration, each isolated variant passed the nine existing crate tests.
Additional remote fixtures checked sampled/unsampled and explicit-parent JSON
correlation, nested fields, no-provider/no-span behavior, filter directives,
text output and Prometheus contents. Separate baseline/candidate resource tests
checked conflicting environment attributes and empty/nonempty instance IDs.
The retained crate tests cover correlation and typed identity; a boundary test
also protects the 200-character prefix against byte-based Unicode truncation.

The source archive, executable hashes, 528 paired timing records, 264 allocation
profiles, behavior receipts and environment details are retained in the task's
local evidence archive. Its SHA-256 is
`95278ea596101464834735fc82c2a54ab2e9b6c1cc9ee23c86d6a7341f7dc0ca`
(205,539,534 bytes). A fresh independent review recalculated the measurements and
verified the behavior receipts without findings. The temporary host was deleted
after the archive checksum and executable hashes were verified. Raw profiles
and temporary cloud scripts are not service source or release dependencies.

## 2026-09-29: JSON layer, static filter, histogram upkeep

Baseline `3c220db`, one DigitalOcean c-4 in `lon1`, the release profile (fat
LTO), both sides built with aligned code (`-align-all-functions=6`,
32-byte branch boundaries). The primary metric is user-space instructions per
iteration, `(I(120k) − I(20k)) / 100k`, which varied under 2% between three
rounds; process CPU per iteration covers kernel time and other threads. The
request fixture is `infra-http`'s server span (three fields, twelve
attributes, W3C parent extraction), three polls, the access-log event, and
close, through `install_subscriber` and `install_tracer_provider` exactly as
the binaries call them, with the default parent-based 10% sampler.

Ablation first: removing the JSON layer removed 61% of a request's
instructions; its flattened event fields cost 20%, the flattened span list
17%, the timestamp 6%, and the trace ids 10%. `json-subscriber` built a
`serde_json::Value` map for the event and another for the span list on every
record and re-serialized a span's fields on every `record`.

| Change | Request instr | Event instr | CPU per request |
| --- | --- | --- | --- |
| Own JSON layer (`logging/json.rs`) | −41% | −62% | −31% |
| + `humantime` timestamp | −45% | −67% | −32% |
| + `Targets` for a directive without span filters | −45% | −67% | −33%; −4 to −7% more at three threads on span-heavy work |

The line is unchanged: a 64-record corpus (every field type, escapes,
non-finite floats, raw identifiers, `log` records, nested spans with
repeated keys, 300 records on one span, explicit and root parents) was
compared byte for byte against `json-subscriber` under five sampler and
provider settings, and the filter under twelve directives, including one
with a span filter. Allocations fell from 76 to 42 per request and from 45
to 13 (4.1 to 1.0 KB) per record; the rest of a record is the OpenTelemetry
span event.

Service end to end (2 Tokio workers on 2 CPUs, stdout to a pipe, `wrk` on
`/health/live` with the access log on): 64 connections 23.0k → 27.3k
requests/s (+19%), 83 → 72 µs CPU per request; one connection 10.9k → 12.6k
(+15%); always-on sampler with an OTLP exporter 18.9k → 21.2k (+12%).
Draining histograms every second instead of ten cut peak RSS from 16.8 to
13.0 MB at unchanged CPU and throughput.

Measured and not retained:

- A non-blocking writer (`tracing-appender`, batched 64 KiB writes): +15–19%
  more requests/s at saturation, but at one connection −16% requests/s and
  +42% CPU per request, since the writer thread wakes for every line; it also
  loses buffered lines on a hard kill and needs a guard held past the last
  log line in every binary.
- Keeping OpenTelemetry span events away from non-recording spans (a
  per-layer filter and a cached span id): −6% for unsampled requests, +10% for
  sampled ones and +6% for every span, from the per-layer filter bookkeeping.
  The layer still builds each event only for the SDK to drop it; that belongs
  upstream in `tracing-opentelemetry`.
- The OTLP export itself is the largest remaining cost of a sampled request:
  with an always-on sampler the exporter thread used 43% of the process CPU,
  more than half of it in `malloc`/`free` while converting spans to protobuf.
  No exporter setting changes that conversion.
