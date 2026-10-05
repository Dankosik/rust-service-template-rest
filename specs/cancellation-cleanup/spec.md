# Cancellation and cleanup: minimal delivery contract

status: ready

Requester meaning: [Intent](intent.md). This specification closes Definition;
it is not implementation or runtime evidence.

## Evidence and behavioral outline

The accepted research is `specs/cancellation-cleanup-research/research/report.md`
in the original checkout
`/Users/daniil/Projects/Opensource/rust-service-template-rest`, SHA256
`c6182a47c272ed05674df7a1d79040871e27c7b00a3cdc814866e228d14704d1`.
Its sibling `review.md` records independent PASS. The report is a static
investigation of `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, not a fresh runtime
receipt. This change starts at `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`;
only authentication, cache, outbound-machine-authentication and production
contract documentation differ between those revisions. The source facts below
were reopened in this worktree; the report need not be available to understand
or implement this contract.

The existing [Download](../../crates/infra-object-storage/src/download.rs)
owns a native SDK body separately from its terminal state. `poll_chunk` drops
the admission/observation owner when it transitions to `Failed`, but retains
the SDK body until the wrapper is destroyed. `bytes(self)` already owns and
destroys that wrapper on error. This establishes a retained-owner gap; no
socket leak or provider-side unfinished operation has been reproduced.

The required delta is eager body disposal at terminal failure, plus one
discoverable set of recipes grounded in the existing infrastructure. All other
research recommendations receive an explicit disposition below.

## R1: terminal download failure releases owned body

When an open `Download` detects a terminal error while being read, it must
release ownership of the original SDK body before returning that error to the
caller. The rule applies to all existing terminal error causes, including
provider-body failure, checksum failure and body-length mismatch. It applies
through `next_chunk` and the `http_body::Body` interface, including when the
caller retains the failed `Download` indefinitely.

At that transition the unverified held final chunk is discarded, the admission
slot is released and the operation's existing failure observation is completed
once. None of this waits for another poll, another operation, wrapper Drop, a
timer, or feature-specific cleanup. No new task, deadline or asynchronous
shutdown API is required.

The failed wrapper preserves metadata and the same closed error. Every later
read returns that same error, never a clean EOF or buffered bytes, and never
polls the original body again. Dropping it later must not produce a second
completion or reclassify the failure as cancellation. Existing error mapping,
public method signatures, size-hint behavior and success/empty-body semantics
are unchanged. `bytes(self)` continues to collect only remaining chunks, to
fail on the same error and to release its owned wrapper on error/cancellation.

The observable resource claim is destruction of this wrapper's native body
ownership. It does not mean a pooled socket closed, SDK tasks joined, or a
remote effect stopped. Retaining some other independent owner elsewhere cannot
be made safe by dropping this wrapper's ownership.

Nearest feasible falsifier: an owned body with a destructor witness survives
the returned terminal error while the failed wrapper is retained. Repeated
error and admission/observation behavior distinguish the intended fix from
silently treating failure as success. Existing adequate coverage is reused;
Implementation chooses the concrete cases and fixtures.

## R2: ready developer recipes with precise cancellation meaning

The maintained developer entry path must make the following recipes easy to
find without reading this research or designing a lifecycle framework. Prefer
the existing [First production feature](../../docs/first-production-feature.md)
and [Object storage](../../docs/object-storage.md) guides and their established
provider links; do not create parallel canonical provider contracts.

1. Ordinary infrastructure operations remain awaited children of their request
   or job. Features use their existing business interfaces; provider adapters
   own the concrete infrastructure calls. The guide explains that dropping an
   owned future drops its owned local resources, whereas dropping a future
   borrowing `&mut` state does not destroy the external owner. It must not
   teach detached `spawn` plus dropped `JoinHandle` as request cleanup.
2. For an object that fits the consuming path's memory budget, the provider
   adapter awaits `storage.get(&key).await?.bytes().await?` as part of the HTTP
   handler's awaited work, then returns buffered data through the feature's
   interface. The existing outer request timeout bounds that work up to the
   response; cancellation destroys the owned download. No additional buffered
   helper or fresh timeout budget is necessary. The guide retains the existing
   distinction between `max_object_bytes`, admission capacity and bytes that
   remain alive in completed HTTP responses. Larger external downloads use
   the existing presigned-GET option when the feature's access policy permits.
3. Direct `Body::new(download)` retains its existing conditional use for a
   promptly reading caller. The HTTP request timeout does not bound response
   transmission after headers, and the provider stall detector does not bound
   an unpolled reader. The guide must not present streaming as the default
   ready option for arbitrary slow clients or suggest R1 fixes that lifetime.
4. Required work that outlives a request uses the existing durable-job path,
   including transactional enqueue where the triggering write needs it. The
   feature chooses stable business operation identity; an external-effect
   adapter uses its provider's idempotency/reconciliation contract. Queue
   fencing and cancellation do not establish exactly-once external effects.
   The existing HTTP idempotency, jobs and transaction guides remain canonical.
5. The guide distinguishes stopped waiting, released local ownership and known
   remote outcome. A timeout/cancellation of SQL COMMIT, S3 mutation or outbound
   write cannot be reported as proof that the effect did not happen. Synchronous
   Drop or abort request does not claim joined asynchronous tasks; already
   started blocking work is not forcibly stopped by dropping an async caller.
   Reuse existing lifecycle owners rather than prescribe new close/join APIs.

Link to existing authoritative provider guidance for API details, budgets,
retry policy and failure mapping. Use the already admitted inbound deadline
where an existing provider API accepts it; do not reset a full request budget
per hop or add deadline propagation to APIs that do not have it. Profiles that
omit an optional capability must not retain broken guide links or unusable
recipes; preserve the repository's current template marker conventions.

Nearest feasible falsifier: a developer following the documented buffered
read must add a new timer/task/cleanup owner, bypass a feature/provider boundary,
or infer remote rollback from cancellation. Static source/guide consistency
and the existing documentation checks are the appropriate proof boundary;
these recipes do not create a new sample application or runtime test matrix.

## Recommendation dispositions

| Research item | Decision now | Reason and reopen condition |
| --- | --- | --- |
| Failed retained Download holds SDK body | Required R1 | Concrete source-supported local resource retention after terminal failure; repair stays with the existing wrapper. |
| Ready usage guide | Required R2 | Makes the accepted infrastructure-owned path usable without feature-specific lifecycle machinery. |
| New helper for buffered HTTP/S3 | Already provided; do not add | Existing owned `get`/`bytes` chain executes under the handler's outer timeout. Reopen only for a demonstrated missing required behavior. |
| Plain SQLx and whole-return backport | Deliberately unchanged | Accepted pool/transaction ownership and bounded local-return mechanism remain authoritative; no new pool facade or database proof is needed for this delta. Backport retirement remains with its existing owner. |
| HTTP/1 EOF, HTTP/2 RST and outbound peer-EOF probes | Conditional; outside this PR | Research found a missing runtime observation, not a defect in the changed path. R1 promises local body disposal and R2 does not promise immediate disconnect recognition or socket closure. Reopen on an observed transport defect or a required claim about those timings/resources. |
| Independent lifetime owner for direct HTTP/S3 streaming | Conditional; outside this PR | No accepted arbitrary slow-reader streaming scenario. Reopen when such a scenario and its capacity/lifetime contract are accepted. |
| New cache/gRPC/library close-and-join API | Conditional; outside this PR | Abort request and task completion differ, but no required integration lifetime is missing for this change. Reopen for a concrete owner requiring awaited completion. |
| New provider-specific reconciliation framework | Outside this PR | Stable business identity and existing provider/transaction contracts are the ready path. Unknown remote outcome remains explicit; a real business effect can select its own reconciliation policy. |
| Broader SQL, Redis, S3 remote-effect, DNS and shutdown experiments | Outside this PR | They do not falsify R1/R2 and would re-prove unchanged behavior or add a new runtime claim. Preserve existing gates; reopen only the boundary invalidated by later evidence. |
| Generic cancellation framework, new crates/configuration, API facade | Not needed | Existing Rust ownership and admitted infrastructure supply the requested paths; no current requirement justifies a parallel mechanism. |

## Outcome, composition and compatibility

A feature adapter performing a buffered object read awaits the existing API
inside the handler. Success returns the same bytes. Handler cancellation drops
the owned download. If a chunk reader instead retains a failed download, R1
releases its body immediately while preserving the error and metadata. Neither
path mistakes local cleanup for rollback or changes business retry policy.
Durable independent work follows the jobs recipe and retains its stable effect
identity across attempts. Thus the ordinary feature path needs no bespoke
cancellation/cleanup supervisor, while unsupported stronger transport or remote
finality guarantees remain explicit.

No schema, migration, dependency, configuration, wire contract, metric family,
retry count, server budget or process shutdown policy changes. No application
facade, new crate, background task or streaming lifetime owner is introduced.
The concrete implementation remains inside the existing download owner and
developer documentation; private representation choices are implementation
details. There is no unresolved runtime boundary, material flow, recovery
mechanism or ownership decision requiring Technical Design.

## Proof boundary and movement

Implementation selects meaningful proof for R1 and reuses adequate compatibility
coverage under the repository's ordinary validation budget. Documentation
checks must cover the changed guides and retained template behavior. No new
mandatory real-provider, socket, database, deployment or production observation
is added. Existing applicable CI/release gates remain intact. Record executed
checks separately from prior static evidence and from the separate PR outcome.

Definition requires independent Specification review. A passing fixed result
moves to Planning; Technical Design is untriggered for the closed owners and
unchanged mechanisms above. Reopen Specification if R1 cannot preserve its
failure contract or new evidence invalidates the scoped outcome; reopen the
smallest upstream owner for a newly required runtime mechanism. Merge and
deployment remain outside authority.
