# Overload and resource hardening

Status: ready. Owner: Definition. Source baseline:
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`.

## Accepted inputs and outcome

[Intent](intent.md) owns requester meaning and authority. The historical
[research report](../overload-resource-isolation/research/report.md) and its
[independent review](../overload-resource-isolation/review.md) are evidence,
not an adopted list of mechanisms. [Recommendation dispositions](recommendation-dispositions.md)
record applicability, competing PRs and deliberate exclusions.

This change closes two remaining template-owned gaps: bounded admission before
gRPC authentication and finite S3 GET custody through confirmed body completion.
It also makes the unchanged workload/resource scopes explicit in the existing
operator guides. No new runtime knob, dependency, pool, queue or scheduler is
required by this contract. Technical Design owns mechanism and placement.

## G1: bounded gRPC opening before authentication

For `grpc.max_in_flight = K > 0`, at most K business openings may concurrently
enter authentication or subsequent pre-header handler work on one composed
router, shared by its clones. The opening count covers all bearer verification
work, including same-token introspection followers and unknown-key JWKS
followers; actual provider-attempt limits are not a substitute. A new opening
must obtain capacity without waiting before any verifier/provider/handler work.

Preserve the existing independent K-cap on authenticated business calls through
terminal response status, failure, caller deadline or cancellation. An admitted
call before headers may occupy both counts. A response head releases its opening
capacity; it does not release terminal-call capacity. Already-open streams do
not consume the opening count, and opening callers do not reserve authenticated
terminal-call capacity before successful authentication. This preserves the
current authentication-before-terminal-admission behavior and avoids spending
all authenticated stream slots on an untrusted slow verification batch.

Both bounds use the existing value K (default 256). At zero both are disabled,
preserving the explicit operator opt-out. HTTP keeps its own bound; neither is
a combined process/fleet quota. There can be up to K openings plus K already-open
authenticated calls; this is not a K-total-futures or memory claim.

When opening capacity is exhausted, return the existing `RESOURCE_EXHAUSTED` /
`server is at capacity` status with catalog reason `SERVICE_UNAVAILABLE`, and
increment the existing shed counter once. Do not authenticate, call a provider,
run the handler or enqueue work. This intentionally gives overload precedence
over malformed/missing credentials for an opening that cannot be admitted.
An admitted invalid credential retains the existing `UNAUTHENTICATED` result;
provider failure remains `UNAVAILABLE`, and insufficient scopes remain
`PERMISSION_DENIED`.

Retain rejected-upload drainage (at most 100 ms and 64 KiB) to avoid the existing
HTTP/2 reset-storm failure. Refused requests doing that bounded cleanup are
outside the admitted-opening count; this change does not claim a bound on all
transport or rejection futures. It adds no waiting queue. Admission failure must
not restart or extend a caller's already established opening deadline. When the
opening deadline has expired before a decision can be made, its existing
`DEADLINE_EXCEEDED` outcome takes precedence. The existing deadline wrapper may
cut short rejection drainage; no separate extra deadline period is added.

Opening capacity releases exactly once on response head, authentication failure,
handler failure/panic, cancellation or opening timeout. Terminal-call capacity
keeps its current body-lifetime owner and unread-response deadline behavior.
After a slow auth batch is cancelled or finishes, a new eligible opening can
progress without restarting the router. Health Check and Health Watch remain
outside business authentication, capacity and deadlines. Their existing shared
socket/transport bounds remain; no new health-Watch quota or timeout is adopted.

Nearest falsifiers: occupied opening slots permit another verifier/handler
entry; same-key followers bypass the count; a returned head retains opening
capacity; opening failure releases terminal capacity it did not own; saturation
blocks health; cancelling the auth batch permanently strands a slot.

## S1: finite complete S3 GET lifetime

`object_storage.operation_timeout` also bounds the complete GET operation,
starting when the async `get` operation is first executed, before admission and
SDK preparation, and continuing through provider headers, every body chunk and
confirmed EOF/checksum decision. Use one original absolute deadline; retries,
response-head handoff and subsequent reads do not restart it. Its existing
default (5 s), accepted range (1 s to 15 min) and configuration key are preserved.
This is an intentional tightening of the existing headers-only GET contract,
not a measured slow-reader throughput limit or a new service-specific quota.

No new SDK dispatch may begin after that deadline. Before terminal completion,
at or after the deadline, GET/Download fails with existing
`ObjectStorageError::Unavailable` and the existing timeout failure class; it must
not decide success or release another payload chunk at that point. Local deadline
expiry takes precedence over a simultaneously observed provider success or body
failure while the operation is still open. A terminal decision made before the
deadline remains final. Ordinary busy admission remains fail-fast and existing
provider/metadata failures retain their mappings when observed before expiry.

The finite lifetime applies to direct `Download` reads, its `http_body::Body`
implementation, `bytes()`, and zero-length downloads whose EOF is verified before
`get` returns. An open Download must release its adapter-owned provider body,
held final chunk, admission slot and operation observation on expiry even if its
owner never polls it again. Merely timing individual reads or releasing the slot
while retaining the provider body does not satisfy this requirement. Timer and
body work are owned and finish/cancel with terminal completion/drop; no permanent
background work or producer queue may survive an ended download. The adapter's
own polling loops must yield after bounded work so ready empty frames cannot
indefinitely prevent the deadline owner from advancing. This guarantee assumes
a functioning cooperative runtime, not preemption of arbitrary blocking code.

Before EOF, repeated reads after failure return the same failure and no payload.
At confirmed EOF before expiry, operation success and slot release happen once;
later reads preserve the existing successful final-chunk/EOF behavior even after
the old deadline. Preserve exact-length, final-chunk and supported checksum
semantics: no complete object escapes before EOF/integrity confirmation. A drop
before completion still cancels and releases promptly without retrying; a drop
after completion does not record another outcome. These rules also govern races
between a body poll, timer, EOF and drop.

If a Download has already supplied HTTP response headers, expiry terminates its
body with the existing error; it cannot replace the sent response status. Bytes
already returned to callers, transport-owned frames and a caller-owned partial
collection buffer are outside adapter custody and may outlive slot release.
This change promises finite active-resource custody, not an RSS ceiling.

Presigned downloads remain the existing choice for remote slow readers; their
expiry semantics are unchanged. A legitimate in-process consumer needing more
time selects an adequate existing operation timeout within its parent budget.
This change introduces no implicit infinite mode or parallel legacy GET path.
PUT/DELETE ambiguity, retries, byte limits, HEAD, readiness probes and presigning
retain their existing contracts. No object is modified by the GET expiry.

Nearest falsifiers: an unpolled Download keeps storage Busy beyond the deadline;
the slot is released but its provider body remains alive; a late EOF becomes
success; a slow pre-header attempt is followed by a fresh full body budget;
zero-length EOF verification hangs; cancelling one read loses the held final
chunk or records duplicate terminal outcomes.

## D1: precise unchanged capacity and workload guidance

Update the existing gRPC and object-storage guides and source configuration
documentation for G1/S1, including defaults, opt-out, failure precedence,
compatibility and scope. Keep source and guide claims consistent; do not rename
existing status/error/metric identities or introduce dynamic labels. Existing
shed and storage outcome/duration observations must distinguish the new failures
through their current bounded vocabularies. No new production metric is needed
solely to replace a deterministic proving fixture.

Record or link the following obligations in existing capacity/provider guidance:

- HTTP counts handlers through response head; large/streamed responses need a
  feature-owned count/bytes/lifetime policy. The template has no such business
  response consumer, so do not impose a universal HTTP body timeout or size cap.
- Auth singleflight/cache retention and token-provider attempt limits do not
  count all waiting callers or resource HTTP. Preserve original consuming
  deadlines through waiting and dispatch. A feature requiring fast/slow-class
  isolation admits the actual resource exchange through EOF, keeping hits outside
  a provider-attempt budget. No separate class is currently established here.
- Keep SQLx native acquisition and its existing return patch, the current HTTP
  100 ms database response reserve, short transaction guidance and the shared
  readiness pool. A consumer's deadline bounds waiting; a future expensive class
  may need fail-fast admission. A second pool or raw `try_acquire` is not a generic
  solution. Cancellation/timeout never establishes rollback or remote completion.
- Jobs already expose `max_running` and webhook delivery configuration, and the
  outbox owns separate execution capacity. Backlog count/bytes/age, expiry and
  per-tenant/endpoint fairness need the service's accepted obligation and replay
  contract; do not delete accepted work or add COUNT-then-INSERT admission.
- Count independent owners and peak replicas, pool maxima plus LISTEN/admin
  allowance, provider work and broker effective pending/retention limits in the
  service production contract. Defaults and cache sizes are not measured safe
  fleet capacity. Existing guidance owns detailed arithmetic/readback.
- Existing CPU/blocking-work guidance and separately owned logging/telemetry
  corrections are recorded in the disposition evidence; this task adds no CPU
  workload or executor and promises no preemption from an async timeout.

## Compatibility, proof and next boundary

No schema, persisted identity, OpenAPI REST response, protobuf, authentication
policy, dependency/toolchain or infrastructure change is accepted here. Optional
profile projections must retain/prune the changed behavior and documentation
with their existing owners. Historical research remains historical. Existing
parallel PRs are neither dependencies nor imported implementation; their specific
unmerged gaps remain explicitly separately owned, not fixed by this PR.

Implementation selects focused deterministic proof for the G1/S1 falsifiers,
matching build and relevant tests under repository validation routing, plus
documentation/profile checks for its actual edits. Final assembled independent
review is required because admission and lifetime affect concurrency safety.
No load/RSS/p99 benchmark, production topology readback or external infrastructure
is required or claimed for this bounded correctness change. A missing required
local/CI result remains pending rather than being inferred from inspected tests
or another PR's receipts.

Technical Design must settle concrete lifetime custody, race/cleanup behavior and
existing-owner placement without broadening this contract. Reopen Definition
for a required new public behavior/knob, a demonstrated incompatibility that
cannot satisfy G1/S1, or source/parallel-PR drift that changes a disposition.
