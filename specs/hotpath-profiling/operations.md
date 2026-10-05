# Hotpath profiling, 2026-10-04

Requested outcome: a measured report and proposed optimizations. No optimization,
push, PR, Railway deployment or Railway configuration change is authorized.
Local work is restricted to source/file inspection and editing, Git, doctl, SSH,
and the explicitly requested rsync transfer. All builds, tests, services,
databases, load generation and profiling execute on the DigitalOcean host.

Approved host: `rust-service-template-rest`, tag `hotpath-profiling`,
`s-8vcpu-16gb-amd` (8 shared AMD vCPUs, 16 GiB RAM, 320 GiB disk), `fra1`,
Ubuntu 24.04, $0.16667/hour. Maximum lifetime: 8 hours, approximately $1.34.
User approved creation on 2026-10-04. Droplet ID: `606013508`.
Public IPv4: `138.68.110.137`. Delete this ID after retrieving evidence and
verify its absence; never delete by the shared tag.

Branch: `codex/hotpath-profiling-20261004`. Baseline is the supplied dirty
working copy, including its existing edits, rather than a replacement from
origin/main. Preserve a baseline source archive and hashes on the host before
transferring profiling edits. Transfer an explicit source-file list; exclude
Git metadata, local environments, credentials, caches and previous evidence.

Methods: repository rust-performance and rust-dependencies, and upstream
hotpath_init at tag v0.28.4. Exact hotpath crate/CLI version is 0.28.4. Keep
profiling opt-in with Cargo features and preserve jemalloc in allocation mode.

Discovery: no service named rust-service-template-rest was returned by Railway
MCP in any of the three accessible projects. This repository is a scaffold:
its normal application router contains health probes and an optional webhook
ingress. Do not infer a production workload from unrelated Railway services.
Choose scaffold workloads and report their representativeness explicitly.

Measurement plan: release build, default and instrumented controls, HTTP/1.1
keep-alive probes and fallback/error routes, JSON access-log and sampling
configuration controls, staged open request rates, repeated saturation samples,
concurrent diagnostics scrape, real synthetic PostgreSQL readiness if enabled.
Keep service and generator CPU affinities separate. Record errors, full-client
latency, throughput, CPU/request, RSS, CPU steal, profiler functions/allocations,
locks, SQL, Tokio and profiler overhead. Query the running MCP endpoint. Final
research report receives one fresh independent read-only review.

Current evidence: ordinary/feature-off/profile release builds completed on the
host. Both 1 KiB and 64 KiB payloads are synthetic; the retained webhook payload
already encodes byte fields as Base64. Three alternating pool-4/pool-16 controls
and three payload-size pairs are retained. Failed MCP-parser and large-payload
generator-initialization probes are excluded, with their raw evidence preserved.
The revised generator produces the same deterministic bytes using SharedArray.

The second profiling candidate adds measurements of receipt admission, payload
preparation/copying, and the existing job-wake mutex. Its release symbols are
retained through `CARGO_PROFILE_RELEASE_STRIP=none` solely for CPU attribution;
the ordinary release profile remains unchanged. CPU snapshots are requested
before the bulk MCP query, while traffic is still running. Remaining work:
The profile and validation are complete. Evidence has been retrieved and its
Git blob hash matched the host. Droplet 606013508 was deleted; the final API
readback returned 404 at 2026-10-04 13:17:47 UTC. Independent read-only review
returned PASS after one bounded correction of two report findings. No required
work remains; optimizations, push and PR remain outside this outcome.
