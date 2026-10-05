# Recommendation dispositions and collision evidence

Status: ready. Definition evidence, inspected 2026-10-05. No tests, builds,
benchmarks, provider operations or CI reruns were executed for this document.

## Fixed authorities

- Current source: `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af` on
  `codex/overload-resource-isolation-20261005`, isolated worktree
  `/Users/daniil/.codex/worktrees/overload-resource-research/rust-service-template-rest`.
- Historical report: [queue/admission/resource-isolation](../overload-resource-isolation/research/report.md),
  source `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, report SHA256
  `a32897f40098f357ec72b336f31b2ca91b88d3b57e0debb714a384d687f4a46f`;
  [research review](../overload-resource-isolation/review.md) passed its bounded
  outbound-resource repair. This is not product acceptance.
- `git diff 5927ffb..78aa3a8 -- crates docs` contains only documentation edits
  to authentication, cache, machine authentication and production contract;
  no product Rust changed. Current gRPC router/terminal owner, S3 Download/get,
  typed config and guide sources were inspected directly; CodeGraph status
  reports this worktree's own index current.
- Current [specification](spec.md) alone owns the accepted delta. Open-PR file
  inventories, relevant patches and immutable head identities below establish
  collision/separate ownership. PR descriptions' validation claims were not
  promoted to this candidate's proof.

## Report coverage

Rows cover the report's resource map, failure scenarios, mechanism comparison,
six minimum recommendations, proving gaps and template/service split. A
service/workload disposition is a deliberate decision for the current scaffold,
not a hidden implementation dependency or a claim that deployment is ready.

| ID / report pressure | Disposition | Current evidence and reason / reopen condition |
| --- | --- | --- |
| R01: gRPC authentication precedes count; minimum recommendation 1 | **Implement here: G1.** Add bounded opening admission before auth while preserving independent terminal-call count. | [router](../../crates/infra-grpc/src/router.rs) `router`, `authenticate`, `shed`, `deadline`; [terminal owner](../../crates/infra-grpc/src/call.rs). No inspected PR changes server router admission. Separate opening and terminal counts reuse existing K; no workload number is invented. |
| R02: gRPC long streams, health Watch and message/aggregate size | **Deliberately unchanged / service decision.** Preserve health exemptions, caller full-call deadline, terminal permit and deliberate no-caller-deadline stream policy. | [gRPC guide](../../docs/grpc.md) names existing behavior. Health Watch is trusted probe protocol under socket/H2 controls; no accepted health-watch SLO/count requiring a new knob. Business generated size setters already exist; #248 documents feature message/aggregate/retained-reader custody. Reopen with a real streaming method or exposure requirement. |
| R03: HTTP header-only permit/timeout, finite relay and retained response bytes; recommendation 3 | **Already adequate for present scaffold; separate existing PR documents feature contract.** No universal HTTP body cap/timer here. | [hardening](../../crates/infra-http/src/harden.rs), [API composition](../../crates/service/src/api.rs). No large/streaming business response consumer is registered. #248 changes `docs/architecture/http.md` and recipes to require feature admission through body completion and poll-independent release. HTTP active gauge already follows body rather than admitted handler count. Reopen for a concrete response consumer. |
| R04: S3 returned streaming body keeps slots indefinitely; recommendation 3 | **Implement here: S1.** Extend existing operation budget through GET EOF and independently reclaim unpolled custody. | [get](../../crates/infra-object-storage/src/lib.rs), [Download](../../crates/infra-object-storage/src/download.rs), [storage guide](../../docs/object-storage.md). #248 explicitly documents indefinite unpolled retention and only changes unread-tail collection plus nonstreaming-response bytes. It does not close lifetime. Presign remains the slow-reader alternative; no new numerical quota. |
| R05: S3 retained collection, preallocated upload and nonstreaming response bytes | **Covered by separate existing PR #248; consumer responsibility remains.** | Its Download patch reserves only unread tail plus held last chunk, and `response_limit.rs` adds provider-response ceiling. Bytes already returned/partial caller collection and PUT inputs remain caller-owned. This PR must not copy those changes or claim them merged; S1 only reclaims adapter-owned active state. Reopen if that PR drops the stated coverage. |
| R06: inbound JWKS/introspection followers; recommendation 2 | **Implement gRPC ingress gap via G1; otherwise adequate bounded ingress, with service-owned class fairness.** | [introspection](../../crates/infra-bearerauthn/src/introspection.rs), [refresh](../../crates/infra-bearerauthn/src/refresh.rs). HTTP already counts auth inside ingress; gRPC opening will count all followers. Cache hit/miss/provider counts and same-key coalescing remain distinct. No promised fairness or public fast-route capacity justifies another global auth quota in this health-only scaffold. |
| R07: outbound OAuth mutex/coalesced followers and expiry before resource dispatch | **Preserve existing consuming deadlines and provider cap; document scopes.** | [OAuth](../../crates/infra-oauth2-client-credentials/src/lib.rs) acquisition uses caller deadline across waiting; `execute` forwards original deadline after authorization. Cache entry/byte limits are retention rather than all live waiters. A new caller outside bounded ingress/jobs must own admission. #247 spreads refresh schedules but does not add follower admission. No new generic follower cap without a consuming workload. |
| R08: outbound HTTP resource work is not token-provider work; recommendation 2 | **Already bounded per operation; class isolation is a service decision.** | [outbound client](../../crates/infra-outbound-http/src/lib.rs) owns required time/header/body limits through complete buffered EOF. OAuth authorization does not hold a provider permit across resource exchange. No actual business dependency with competing fast/slow classes is registered; D1 requires actual-resource EOF custody when a class is adopted. #242 separately closes the absolute-deadline setup/late-success edge; do not claim that fix on main. |
| R09: outbound webhook occupies ordinary job slots; recommendation 2/5 | **Already adequate extension point; service value unchanged.** | [Dispatcher::register](../../crates/infra-webhooks/src/outbound.rs), [outbound guide](../../docs/outbound-webhooks.md): `webhooks.max_concurrent_deliveries` is already wired to `Policy::max_running`. Default None is explicit; endpoints share a kind and bounded complete-body client. No business competing-kind capacity target supports an invented default cap/per-endpoint scheduler. |
| R10: SQLx waiting callers, request reserve, transaction lifetime; recommendation 4 | **Keep native mechanism; service class decision plus existing guidance.** | [pool](../../crates/infra-postgres/src/pool.rs), [persistence](../../docs/architecture/persistence.md), [HTTP reserve](../../crates/infra-http/src/harden.rs). Pool is bounded, ingress/jobs count callers, SQLx acquire is finite, request paths already use deadline minus 100 ms. A native connection `try_acquire` cannot replace establishment; another pool needs measured reserved-capacity need. No known expensive business class exists. #243 bounds startup session readback; it does not establish a waiter count and is not presented as doing so. |
| R11: SQLx silent return, saturation readiness, recovery | **Existing return repair adequate; preserve policy. Separate startup/readiness repair #243.** | [SQLx patch](../../vendor/sqlx-core/PATCHES.md) bounds whole native return at 5 s on current main. [real-Pg tests](../../test/tests/postgres.rs) provide existing test surfaces, not new pass evidence. Shared-pool readiness deliberately can withdraw the replica; no isolated readiness pool or different threshold is adopted. Cancellation/timeout remains outcome-unknown at the database effect boundary. |
| R12: jobs local concurrency, durable backlog, expiry/fairness; recommendation 5 | **Existing active bounds adequate; service-owned backlog/obligation policy.** | [background jobs](../../docs/background-jobs.md), [async architecture](../../docs/architecture/async.md), `claim.rs` only claims free slots and supports per-kind bounds. Existing dedicated outbox engine remains. Queue count/bytes/age and endpoint/tenant priorities require accepted arrivals/storage/replay/freshness policy; naive COUNT then INSERT is not fleet admission, and expiry cannot erase an unresolved accepted obligation. #240 preserves queue/lease policy while repairing payload panic/effect guidance. |
| R13: JetStream delivery reservations, pending ACK, broker backlog | **Native local reservation already adequate; abandoned SDK work separately #248; effective broker policy separately #239/service-owned.** | [consumer](../../crates/infra-messaging/src/consumer.rs) limits active plus reserved pull count and bytes. MaxAckPending is shared durable unacked work, not total pending/storage. #239 docs require effective limits/retention/capacity readback; #248 adapter/native patch retains outstanding publish/request ownership. No broker topology mutation or distributed semaphore here. |
| R14: Tonic outbound Buffer / callers awaiting readiness | **No extra application queue. Current caller deadline retained; service-owned active/stream sizing.** | [client](../../crates/infra-grpc/src/client.rs) already has FullRpc and explicit OpeningOnly policies. Native buffer storage does not bound all waiting futures or stream lifetime. No registered business outbound RPC demand establishes another cap. #246 repairs retained full-dial timeout; no additional buffer or active knob is necessary to G1/S1. Reopen with a concrete dependency workload. |
| R15: CPU/blocking work admission before submission and actual completion; recommendation 6 | **No product CPU workload, no executor addition. Separate #244 owns native upload yielding and adopted guidance.** | [runtime lifecycle](../../docs/architecture/runtime-lifecycle.md) and #244's `Business-work admission and lifetime` patch cover queued+running permits, cancellation, completion/panic custody and shutdown. No Rayon/pool or blocking-thread tuning. S1 separately requires bounded Download polling so its own timer can run; upload logic is not copied. |
| R16: synchronous logging / bounded telemetry / diagnostics | **Covered by separate existing PRs #244/#245; no duplicate implementation.** | #244 changes `logging/output.rs` and runtime custody; #245 overlaps that subsystem with byte/record bounds and privacy. Their integration conflict is outside this PR. This branch keeps existing observability and does not claim these fixes are merged or that diagnostics have reserved CPU. |
| R17: local vs fleet accounting, connection/LISTEN budgets; section 9 | **Existing arithmetic adequate; D1 consolidates pointers and limitations.** | [connection allocation](../../docs/architecture/persistence.md#connection-allocation) already includes rolling overlap, API/worker pools, LISTEN, admin and pooler distinctions. [production contract](../../docs/production-contract.md) owns service capacity inputs. OAuth/auth source docs already distinguish replicas. No live fleet quota or distributed limiter required without service target. |
| R18: overload/recovery measurement matrix and missing gauges; sections 7–8 | **Bounded correctness proof here; workload measurements not justified now.** | Implement the G1/S1 deterministic falsifiers and recovery with existing fixtures/observations; executor chooses tests. Existing tests were inspected, not run. p99, heap plateau, fleet limits and fault recovery SLO require a workload/environment, so no load generation, new infrastructure, expanded telemetry taxonomy or benchmark claim. |
| R19: library alternatives and generic admission framework; sections 5–6 | **Reuse current owners; additions unjustified.** | Research compared Tower load shed/global limit/Buffer, Tokio semaphore, SQLx native pool, Tonic Endpoint, AWS SDK, existing outbound client and jobs per-kind caps. Existing native admission and lifetime tools match the two gaps. No new library is required; Technical Design must examine native extension points and reuse the current owner before any custom custody mechanism. |

## Immutable parallel-PR evidence

All were OPEN when inspected. These are independent tracks, not prerequisites
to building this branch and not proof of current-main behavior. The table records
the exact head read by `gh pr view`; relevant `gh api .../pulls/N/files` patches
were inspected for the behavior-bearing overlaps described above.

| PR | Head | Evidence actually used |
| --- | --- | --- |
| [#243](https://github.com/Dankosik/rust-service-template-rest/pull/243) | `d200ef09ee88014995e2b07515a16340d049459b` | `pool.rs` adds session verification/close deadlines; no pool waiter-count change. |
| [#248](https://github.com/Dankosik/rust-service-template-rest/pull/248) | `7420f7c37ab036c2642551a5e50d2ec7dfb1b737` | `download.rs` unread-tail allocation; `lib.rs` response-limit interceptors; HTTP/gRPC/storage guide patches. No GET lifetime timer. |
| [#244](https://github.com/Dankosik/rust-service-template-rest/pull/244) | `0f623b7355f53a4c0a728afa095b3085b808f6d7` | `body.rs` upload poll budget and runtime lifecycle pre-submission/actual-completion guidance; logging inventory and description. |
| [#245](https://github.com/Dankosik/rust-service-template-rest/pull/245) | `7e88433c546ae689f7464f15310154800034ba6c` | File inventory and description establish independent telemetry/logging overlap; no executable claim consumed. |
| [#246](https://github.com/Dankosik/rust-service-template-rest/pull/246) | `7a337196d0cd44b998a4bb19ff1816402523a8cc` | gRPC client connector patch bounds full dial; outbound HTTP patch is TLS documentation. Server router unchanged. |
| [#247](https://github.com/Dankosik/rust-service-template-rest/pull/247) | `01ebf13070be8a1542413e83ca902c8138c17383` | JWKS/OAuth source patches spread refresh scheduling; no all-follower admission. |
| [#239](https://github.com/Dankosik/rust-service-template-rest/pull/239) | `7223ea877f031d440842d3df6876857e91492ec2` | Durable-messaging patch records effective limits, storage/capacity/retention and recovery readback. |
| [#240](https://github.com/Dankosik/rust-service-template-rest/pull/240) | `acf07e894cf70461013019b9351845b5a53800da` | Inventory/description confirm separate payload-panic/effect-identity scope, preserving queue/lease policy. |
| [#242](https://github.com/Dankosik/rust-service-template-rest/pull/242) | `b0f9899dd90ca1e1002e4dbf7575e3799a381e08` | Outbound HTTP patch fixes one absolute end across setup, dispatch and buffered completion. |

Refresh only affected dispositions if main, these relevant heads, or the accepted
scope changes before implementation/delivery. Do not cherry-pick another PR to
make a disposition look complete. S3 files and operator guides may later conflict
with #248; preserve both independently accepted behaviors during an authorized
integration, or reopen the smallest owner when their semantics disagree.
