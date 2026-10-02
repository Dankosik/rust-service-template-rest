# Cache reliability completion

Status: Accepted locally; separate PR publication and CI remain pending.
Baseline: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Unit: [implementation](implementation.md), accepted behavior [R1–R5](spec.md).

The implementation checkpoint `4ee53a6` was merged with current main
`546a381` at `ed9920510ff25c8d5081652e7721de38afdba501`, retaining its Rust
1.99 upgrade and two mechanical cache test/documentation edits. No merge
conflicts occurred. Final reviewed source tree:
`b91a87cff3a75738c852c869ce050610fc9fcacb`. Subsequent changes to this
completion receipt do not change the reviewed implementation or tests.

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

All mutable lanes joined before final validation. Original documentation lane:
`/root/cache_implementation/cache_docs`; protocol tests lane:
`/root/cache_implementation/cache_regressions`. The Lead owns production,
assembly, final proof, review repair and local acceptance.
After interruption, delivery resumed under `/root/cache_delivery_resume`.
Its bounded scheduler repair lane
`/root/cache_delivery_resume/refresh_cadence_repair` joined before final
revalidation and review; there are no remaining mutable writers.

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

Historical raw log: `/tmp/cache-closeout-before.log`, SHA-256
`4e8605e6a923c892ef5a3205c1ecf5382f52fa8c77d699def5ccb4357a7332f4`.
Baseline test source SHA-256:
`3e11bd23977bb99c8580e53a68fcff349931abf95162b63b533b1825b4db7671`.
The raw log disappeared across the host restart. The failures above were
directly observed in the original session and retained as session evidence;
they are not represented as currently rereadable raw output. The baseline was
not rerun merely to recreate that log.

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

The non-overlapping plan ran serially under the Git-common validation lock.
Environment: `rustc 1.99.0 (b940084d7 2026-09-28)`, aarch64-apple-darwin,
LLVM 23.1.1; PATH included `/Users/daniil/.cargo/bin`, `/opt/homebrew/bin`
and `/usr/local/bin` (Docker). `CARGO_TARGET_DIR` reused
`/Users/daniil/Projects/Opensource/rust-service-template-rest/target`.
Every build/test Cargo invocation used `--locked`. No heavy/full mode, new
environment or shared-cache cleanup was used.

| Check on merged candidate `ed992051` | Observed result |
| --- | --- |
| `make fmt-check` | PASS, 1.07 s. |
| `make docs-check` | PASS, 1.29 s; 880 links, 387 unique, 753 OK, 127 excluded, zero errors. |
| `make build` | PASS, 10.37 s. |
| `make test` | PASS, 95.69 s; 810 passed, zero failed, one ignored Go wire fixture owned by CI. Includes 44 cache unit tests and one cache doctest. |

Independent review found F1: refresh scheduled five seconds after completion
of the previous read/AUTH could exceed the accepted seven-second recovery
bound. The repair schedules from refresh admission, retaining serial ownership,
credential priority and no accumulated ticks. The existing rotation test now
delays AUTH replies by 900 ms and selected PING replies, observes server
rejection before enabling the unchanged password, and requires client-observed
AUTH success within seven seconds on the same connection without user traffic.

The identical strengthened test failed on pre-repair production at that
seven-second assertion (one failed, 43 filtered, 12.01 s scenario), then passed
with the repair (one passed, 43 filtered, 20.92 s scenario). Observed retained
connection recovery was 5.905499792 s. No test-only production seam was added.

| Refreshed proof on reviewed tree `b91a87c` | Observed result |
| --- | --- |
| `make test-package PKG=infra-cache` | PASS; 44 unit tests in 21.32 s and one doctest. |
| `make fmt-check` | PASS, 1.01 s. |
| `make build` | PASS, 4.10 s. |

The scheduler-only repair invalidated the affected cache runtime/build proof;
that proof was refreshed above. Unaffected workspace and documentation results
are reused. Final receipt-only edits receive a fresh documentation link check.
Raw logs, SHA-256 hashes and candidate/environment receipts remain under
`<git-common-dir>/codex/cache-closeout-final-20261002/` in `receipt.json` and
`f1-receipt.json`. The F1 before/after/package logs are retained there, rather
than only under `/tmp`.

The pre-main-merge attempt's old-toolchain formatting/build results are not
used for final acceptance. Its workspace test was interrupted for integration;
its docs check failed because that invocation omitted Docker's PATH. Earlier
compile-only diagnostics, including the disk-space failure, likewise do not
substitute for the passing final runs above.

## Independent delivery review

Fresh reviewer `/root/cache_delivery_resume/delivery_review`, selected through
native Codex controls as `gpt-6-astra` / `high` with no inherited turns,
reviewed the whole assembled R1–R5 outcome and returned FAIL only for F1.
The same reviewer performed the bounded repair recheck and returned **PASS**
on tree `b91a87cff3a75738c852c869ce050610fc9fcacb`, with no surviving finding
and no reopen owner. It inspected the failing/passing F1 logs, cache package
proof, refreshed build/formatting and retained workspace/docs receipts.
Unaffected R1–R5 reasoning remains in scope; tests alone do not supply the
independent verdict.

## Delivery boundary

Remote push, one separate PR and exact-head CI readback remain with the root
continuation owner. No merge or deployment is authorized or claimed.
Valkey, initializer/profile, image and all other selected external gates remain
CI-owned. Local acceptance does not establish those results or deployed runtime
behavior. The worktree and accepted decision packet remain available for this
outstanding delivery.
