# Cache reliability completion

Status: Implemented; final validation and independent delivery review pending.
Baseline: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Unit: [implementation](implementation.md), accepted behavior [R1–R5](spec.md).

## Implementation

The private connection owner replaces manager swapping and detached warm-up
with one cancelled/aborted supervisor, canonical multiplexed generations,
identity-fenced retirement, absolute command deadlines, independent setup and
bounded periodic PING. User commands are dispatched once. Password refresh
uses direct AUTH, retains authenticated rather than merely readable bytes,
and retries rejected unchanged bytes without traffic. Streaming credentials
and their raw dependency logging path are removed.

The crate keeps admission, TLS, the bytes-only API, optional readiness and
service dependency-stage drop. Documentation now places provider calls in
composition/adapters and permits Moka copies across replicas. Resolved redis
remains 1.7.1; no package versions changed. Existing backon and tokio-util own
backoff and cancellation; tracing-log/subscriber are test-only edges for the
actual dependency log bridge.

All mutable lanes joined before final validation. Documentation lane:
`/root/cache_implementation/cache_docs`; protocol tests lane:
`/root/cache_implementation/cache_regressions`. The Lead owns production,
assembly, final proof, review repair and local acceptance.

## Regression evidence

Pre-fix code was restored from the baseline's exact `lib.rs` and
`credentials.rs`, original runtime manifest/features and lock resolution,
with only the two test-only log bridge dependencies added. Source remained
fixed during `cargo test --locked -p infra-cache --lib reliability_ --
--test-threads=1`, under the Git-common validation lock. All six tests compiled
and failed for their intended runtime assertions; execution took 35.33 s.

| Original regression | Observed baseline failure |
| --- | --- |
| Bridged AUTH diagnostics | Unique raw server marker and synthetic rejected credential appeared in rendered redis dependency log. |
| Cancelled long command/probe | The silent socket retained cancelled response slots. |
| Final owner during setup | Socket stayed alive beyond the 500 ms cancellation observation, awaiting setup/warm-up. |
| Namespace/probe ownership and maintenance | No independent maintenance PING reached the socket. |
| Unchanged rejected password | No later authentication succeeded without another file change or traffic. |
| Repeated stalls/no write replay | Stalled generation was neither replaced nor released. |

Local raw log: `/tmp/cache-closeout-before.log`, SHA-256
`4e8605e6a923c892ef5a3205c1ecf5382f52fa8c77d699def5ccb4357a7332f4`.
Baseline test source SHA-256:
`3e11bd23977bb99c8580e53a68fcff349931abf95162b63b533b1825b4db7671`.

The final tests extend that evidence with a deliberately suspended old waiter
resumed only after a successor serves traffic, later unreadable-file recovery,
exact password case/space/CRLF and default/explicit username semantics, and
rejection of premature credential-success logging. The existing setup retry
test now observes the seven-attempt chain and recovery without driving GETs.
Existing admission, TLS, operation metrics, idle disconnect, READONLY, service
process and new-connection rotation coverage is retained.

Removed credential tests owned only the superseded Watch stream: initial
subscription/change yielding and retry-after-first-read. Their meaningful
admission/newline/rotation/outage behavior is exercised at the public protocol
boundary; stream item sequencing and private Watch state have no remaining
production owner. No test-only production seam was added.

## Final validation

Selected non-overlapping local plan: formatting check, workspace build and
workspace tests (manifest/lockfile changed), then documentation link check.
Run serially under the Git-common validation lock on the assembled candidate.
No heavy/full mode and no new environment. Valkey, initializer/profile, image
and remaining selected external gates belong to the PR's CI run.

A compile-only production diagnostic passed during coding. A later compile-only
attempt failed before checking this code because disk space ran out; it grants
no proof. Reuse existing build artifacts for final validation without clearing
shared caches. Final command results and reviewer disposition are pending.

## Delivery boundary

Remote push, one separate PR and exact-head CI readback remain with the root
continuation owner. No merge or deployment is authorized or claimed.
