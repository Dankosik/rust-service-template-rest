# Remaining optimization execution

Authority: user requested all remaining opportunities from the profiling report
after the accepted body-transfer optimization: payload serialization, span/
metric overhead, SQL exchanges, and deployment-specific pool sizing. Root owns
infrastructure, evidence custody and final cleanup. This bundle must account
for every named area; it cannot silently narrow to a single easy change.

Current branch: `codex/hotpath-optimizations-20261004`. Preserve previous
accepted optimization and unrelated dirty work. No push, PR or deployment is
authorized. Railway remains read-only; no production secrets/data or paid
provider calls. Code transfer uses a reviewed rsync allow-list, never credentials.

All builds, tests, databases, services, generators, hotpath and executable
measurement reduction run only on an approved DigitalOcean droplet. Local work
is source/document reads/edits, Git and DigitalOcean control via doctl/ssh/rsync.
The previous droplet 606044304 is deleted. User separately approved this new
host: same repository name, hotpath-profiling tag, 8 AMD vCPU /16GiB, fra1,
$0.16667/hour, max8hours/about$1.34. Root deletes its exact ID after evidence
retention; no approval for another future paid host is inferred.

Use hotpath0.28.4, feature-gated instrumentation and jemalloc as before. MCP6771
and panel6770 remain loopback and use SSH forwards. No hotpath agent init.

The accepted body-transfer candidate is the new baseline. Root preserved
immutable source custody before remaining production-code edits. Definition,
Technical Design and Planning are ready with independent PASS reviews.
Root now owns the Implementation ledger and remote Completion; Leads own only
their named source/test scopes.

User approved the new paid resource. Created droplet **606085569**, public IP
**46.101.222.57**, exact agreed size/region/tag at `2026-10-04T17:57:04Z`
(20:57:04 Moscow). Eight-hour limit ends `2026-10-05T01:57:04Z`
(04:57:04 Moscow). Delete this ID only. Root owns SSH/execution.

Source copied with `source-files.nul` into `/root/remaining/baseline`, archived
before edits, tools installed and ordinary/profile baseline binaries ready.
Public source/config fixtures only; the credential-name matches in the list
are code modules, not secret data, plus public `tools/versions.env` pins.
`.git`, local credentials, target and CodeGraph data are excluded.

Immutable working-copy baseline captured on the approved host before any
production edit: `/root/remaining/evidence/baseline-source.tar.gz`, SHA-256
`aae64f3624831b7ef5a60491810d812a3034511f346b7811de2b88d6b0c0e2e7`.
Verified source blobs: accepted inbound `ad981d30e1590dcb6ea0b31842f633b288626965`,
HTTP observe `4cf33813623f368e62aaad0424d00719af62f7b8`, jobs enqueue
`a1da3f1b40051dfc4c3c0347d9622a69905467ef`; Cargo.lock blob
`981eb9ec797d1d23d6d6effabc21d5a100ede398`.
Bootstrap/setup session37197 completed at `2026-10-04T18:26:31Z`.
CLI hotpath0.28.4 confirmed. Ordinary/profile builds were sequential; profiler
features include supported exact Prometheus counters alongside MCP. Binary
manifest is `/root/remaining/evidence/baseline-binaries.sha256`:
ordinary `2f9c608afb17692b9a14b805ef3ac28ecec57e5ee6da3265087cdf2d89e754af`,
profile `d9f9352e555221c394a36ea3e91737dfb9f2f34685c07dc184da0617c77e4d79`.
MCP/panel SSH tunnel session37869 forwards6771/6770 and returns16771 for the
remote MCP client; all listeners stay loopback.

Observed PostgreSQL budget: max_connections100, reserved_connections0,
superuser_reserved_connections3, fsync/synchronous_commit on. Synthetic app
role is fixture superuser; budget still conservatively leaves reserved slots
unused. Admission benchmark topology: one service, no processing worker or
LISTEN connection, bounded observer sessions, no concurrent migration/rollout.
Retain `/root/remaining/evidence/pool-budget.txt` and process-topology receipt.

Protocol observation ran serially on the ordinary baseline with pool1 through
a loopback TCP proxy. It retains frontend control/execute/Sync headers and
ReadyForQuery operation groups, never passwords, bind/row/message payloads.
Warmed new-with-wake shows5 request exchanges, debounced new4, duplicate3;
each then has a separate empty Sync/ReadyForQuery for pool return. Return
events cross HTTP phase labels, so final reduction must use connection/command
sequence rather than assign them by the displayed phase alone. Startup/prepares
are separated. Controlled-proxy latency is not direct-service performance.

Both implementation units assembled, all writers stopped. Formatted candidate
blobs: inbound f1e3cc4769d97b56be6bea1855a305a2a0a1c21e, observe
da5d891fae74dab9f472e735545fa27e487f9cb5, test37ed0d081960012dde17cf97751f42fd292f6e1d.
Fresh build/fmt/ordinary and profile release proof passed;806 ordinary and204
PostgreSQL tests pass with one existing CI-owned ordinary ignore. Matching
actual configured JSON response/log parity passed. Campaign runs sequentially
on those fixed binaries; no source/feature/dependency change in measurements.

Before follow-up results, root selected exactly3 additional paired ordinary
small-webhook and SQL-free-HTTP observations, same5+30s and source/parameters,
ordered C/B,B/C,C/B. Reason: initial small RSS B23.969–25.375MiB versus
C26.625–29.375MiB; HTTP initial peak ranges narrowly separated too. This
specific regression question is not waived by allocation-byte savings.
All initial samples retained; no reruns of unaffected large/duplicate/mixed.
`scripts/rss-followup.sh` starts only after campaign/pool cells complete.

## Repair and final custody

First final review rejected display-name formatting for six paired HTTP peaks
above baseline; the original FAIL and every sample are preserved. After three
ordinary name-isolation pairs, T2 restored only eager name construction.
Final source observe blob002a082fdf8ee704905926c244418d2e27ff7b0c; matching
fmt/build and81 HTTP tests pass, configured JSON/response parity passes.
Root reused unaffected payload/database proof and ran three fresh direct
baseline/final HTTP pairs plus nine final-source pool4/8/16 sizing cells.
Completed at20:43:03Z; all87 cells have successful process receipts, no invalid
cells or exclusions, zero unexpected response failures. Drops are retained.
Final synthetic selection8 is based on the final-source cells only; historical
rejected-source pool variability stays in the report. Same final reviewer
receives a bounded delta recheck; root owns archive transfer and exact-ID cleanup.

## Terminal operations receipt

Bounded recheck PASS; source/runtime acceptance boundary unchanged. Archive
259MiB transferred; SHA2562e3170a3f2a07267fbf4cf1fa6632e781e297d4c5c1cbc59a0baa979753630c0
computed remotely, local/remote Git blob84e5cad2a72fb47af027f7b0a8c9fe84333962ac
matches. Separate frozen baseline archive identity matches too. Readable final
docs receipt119total/70unique,104OK/0errors/15offline exclusions is retained
outside the earlier archive; all task shell scripts pass shellcheck.

Only606085569 was deleted, request2026-10-04T21:01:48Z. First immediate get was
stillactive; subsequent readback API404 confirmed2026-10-04T21:02:22Z, request
6fd549f8-6e90-49f9-a000-08d9894c0d99. Metadata/stdout/readbacks preserved beside
the report. Elapsed3h05m18s (within8h), proportional estimate≈$0.52. Exact own
SSH-N tunnel to46.101.222.57 terminated after remote deletion; no shared-tag
resource deletion, other checkout edits, remote Git writes or Railway writes.
