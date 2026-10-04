# Optimization execution host

User authorized a new DigitalOcean host on 2026-10-04: 8 AMD vCPU / 16 GiB,
`fra1`, $0.16667/hour, maximum 8 hours (about $1.34). Root owns provisioning,
remote commands, evidence retention and final deletion. No deployment, push,
PR, production mutation, production data or credential transfer is authorized.

Droplet: `rust-service-template-rest`, ID **606044304**, IP **159.89.101.4**,
tag `hotpath-profiling`, Ubuntu 24.04. This ID must be deleted after evidence
retrieval; never delete by the shared tag. All builds/tests/database/load/
profiling and executable result reduction happen only on this host.

Branch `codex/hotpath-optimizations-20261004` preserves the dirty profiling
instrumentation and unrelated source edits. Before candidate mutation, root
copies a source allow-list into an immutable remote baseline and archives it.
Definition owner writes intent/spec and its review/transition; root writes only
this infrastructure state until a reviewed phase transition closes.

Created at `2026-10-04T13:56:27Z`; the approved eight-hour limit ends at
`2026-10-04T21:56:27Z`. Snapshot SHA-256:
`03989134f24224b2ab95709881ee5ec5d89f7c8e3dabe06ad0b778bccf23d82f`.
Baseline ordinary/profile binaries were ready at `2026-10-04T14:22:56Z`.

Candidate writers stopped. Source blobs (inbound, HTTP, integration test):
`ad981d30e1590dcb6ea0b31842f633b288626965`,
`64bce8377a68db1ecb97c637159c8afc466a8910`,
`184aec41c05b7cd26ce9d1d22476b7549eb01143`.
Bounded production/test compile diagnostics passed. One formatting-only repair
was made before final validation; production blobs were unchanged.
Root runs `scripts/validate-candidate.sh` remotely, with logs under
`/root/optimization/evidence`; checkpoint: completed build/test stages or active
compiler progress in their logs, followed by candidate-ready receipt. No load
comparison overlaps these checks. The final independent reviewer is
`/root/optimization_final_review`; code inspection has no anchored findings,
with final validation/comparison still pending.

Ordinary comparison completed at `2026-10-04T15:23:21Z`. All 24 retained
ordinary controls have correct responses and no dropped iterations. One early
small baseline cell copied the preceding load script while optional fixed-ID
support was staged; it was excluded by provenance before outcome inspection,
retained with [superseded-cell.json](superseded-cell.json), and replaced by
`baseline-small-r1-matched` after the original series. Its order is later than
the original candidate cell, not the original B/C order.

Precision follow-up uses hotpath 0.28.4's built-in `hotpath-prometheus` cargo
feature, with no manifest or lock change, matched on baseline and candidate.
The already-resolved `prost` dependency supplies its protobuf endpoint. MCP
remains active; additional loopback `6772` counters provide exact per-function
bytes, allocation counts and calls. No upload/cloud mode is used. Before/after
counters exclude warmup, and the two precision cells use equal padded ID lengths.
`scripts/build-raw.sh` starts only after all initial loads stop.

The initial three mixed ordinary cells show peak RSS medians 23.625 and
24.078 MiB, with narrowly separated peak ranges. Candidate after-warmup RSS is
already about 0.25 MiB higher; end RSS overlaps, which does not explain all the
transient peak difference. Before any follow-up result, root selected exactly
three additional matched 30-second mixed pairs at 800 RPS/pool4, ordered C/B,
B/C, C/B, in `scripts/mixed-rss.sh`. They start after precision builds and
loads, preserve every initial observation, and address only this unresolved
RSS regression question. No other ordinary workload is repeated.

All measurements complete at `2026-10-04T15:41:09Z`. Exact target passed:
416,165 → 350,599 bytes over construction/preparation; 65,566 bytes and one
allocation saved. The RSS follow-up did not reproduce disjoint peak ranges;
the remaining positive candidate median and all observations are disclosed.
Fresh final independent Implementation Review: PASS (`review.md`).
Evidence archive transport checked by equal remote/local Git blob
`4e6449882478746dd82cb9db8c0965ae1f2a1b7d`; SHA-256
`f50e41e5cd767696cdfb9616744e109d7ca39d0cab67acabcb50a9f03de61dd7`.

Deleted exact droplet `606044304` after evidence retrieval: deletion request
accepted at `2026-10-04T16:07:17Z`, provider API returned 404 at
`2026-10-04T16:08:42Z`. Local SSH tunnel closed. Confirmation retained in
`.artifacts/hotpath-optimizations/deletion-readback.txt`; no tag-wide deletion.
Approximate lifetime 2h12m and hourly arithmetic cost $0.37, under $1.34 cap.
