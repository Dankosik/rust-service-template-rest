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
