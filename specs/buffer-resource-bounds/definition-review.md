# Definition specification review

Reviewer: `/root/definition/specification_review`, fresh read-only collaboration
agent, native `gpt-6-astra` / `high` selection accepted; active identity observed.
Method: [Specification Review](../../docs/spec-first-workflow/phases/specification-review.md).
Baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

## Initial candidate and result retained for unaffected scope

- candidate: intent SHA256 `09c84aff42d8fae6d2f2201b4f40c8c91e576def65d120b766b376337cda6cc2`,
  spec `20b88281b8816686185e0d5b1238edfb58d13a87f00c3a894105d2581cccd3be`,
  research `138b82401e8434727bcbb0a182fb4bfa78bb4b50659101cc64706f5a8af86e49`.
- verdict: PASS.
- findings: none; F1 closed.
- evidence_boundary: fixed documents and relevant current source; one bounded
  S5 repair recheck, with unaffected initial review reasoning retained. No runtime
  validation or implementation correctness claim.
- reopen_owner: none.

The initial review returned FAIL for F1: the publication formula did not enforce
the receiving resource's M against independently prepared events, while malformed
DLQ payloads may exceed M. Repair explicitly rejects oversized source/outbox
payloads and final headers before dispatch, and preserves DLQ transfer under its
separate broker-size/stream policy. The reviewer verified native async-nats 0.50.0
checks header-plus-payload size before ACK admission and command enqueue.

Attempted falsifiers now closed: cross-resource event preparation; trace-injected
headers above H; malformed source payload transfer; source ACK before DLQ success;
and falsely claiming a 64 MiB shared DLQ window. S1–S4 explicitly preserve held
final chunks, exact oversized job byte counts, late serialization-error precedence,
and ownership of cancelled unanswered cache commands. Smithy API citation corrected
to the lockfile's 1.18.0. No unresolved material behavior divergence survives.

After PASS the phase owner changed only spec status from draft to ready. This
mechanical lifecycle update does not change the reviewed semantic scope.

## Current S2 delta review

S2 was reopened after Technical Design verified that PUT/DELETE collect even 2xx
bodies before checking Error XML. The initial S2 error-only scope is superseded;
all other accepted behavior and prior review reasoning remain unchanged.

- candidate: spec SHA256 `d284cbc5ae0a79685fddd9c654a43dcc6d8db0f62c093d6e0cfd5f69a243718a`,
  research `0a488acf774ee51781d4baa8c687b425a2916a9e2f8140a35cdc2f46c4d3a05c`,
  intent unchanged `09c84aff42d8fae6d2f2201b4f40c8c91e576def65d120b766b376337cda6cc2`.
- reviewer: `/root/definition/s2_delta_review`, fresh read-only collaboration
  agent, native `gpt-6-astra` / `high`; accepted selection and running identity
  observed before its result.
- verdict: PASS.
- findings: none.
- evidence_boundary: reopened S2 only, fixed documents, Cargo.lock and resolved
  SDK source; unaffected prior review retained. No runtime/implementation claim.
- reopen_owner: none.

Attempted falsifiers: 2xx PUT/DELETE collection bypass; an empty HEAD response whose
Content-Length is object metadata above 1 MiB; a successful GET above 1 MiB;
fragmentation, dishonest lengths, exact-boundary truncation and empty-frame
retention; oversized/unreadable 2xx mutation falsely reported as success or definite
refusal; SDK error parsing being replaced. All are excluded by revised S2. In
particular, the ceiling counts actual response body data, so HEAD metadata size
alone cannot trigger refusal. Successful GET keeps its independent object bound
and EOF/checksum semantics. Mutation response loss remains OutcomeUnknown.

After this PASS, only the spec lifecycle status changed from draft to ready.
Current ready spec SHA256: `37c032b79b70f6a73bbb2eb2c3fd9ffcc824eeef2328658cfb813994edf62e6c`.

## Same-behavior S5 evidence and source-policy refresh

Technical Design confirmed that stock async-nats 0.50.0 native controls leave
pending request-map and polled ACK-future cancellation lifetime gaps. Definition
clarified the no-upgrade constraint: an evidence-backed narrow source lifetime
repair at the same version is admitted when native APIs cannot meet S5. The
research disposition now records the gap and routes the exact repair, evidence,
proof and removal condition to Technical Design under the existing dependency-patch
pattern. Native Context admission, ACK parsing and automatic recovery remain owned
by the dependency. No public protocol, task/queue system or memory manager is added.

This changes evidence/source-policy disposition only: S1–S6 behavior, numerical
limits, rejection/ambiguity, proof scope and the current S2 refresh are unchanged.
Under Transition's unchanged-semantic-scope rule, the prior PASS remains valid for
those behaviors. This is not a review verdict for a proposed vendor patch; its
mechanism and implementation remain subject to their own current phase boundaries.

Current ready spec SHA256: `cace772a9cd747bfe414855f5520160960407dc6998aac8cffd472633b52b507`.
Current research SHA256: `4534602d30635c993e40e21726ad501e7bdcdd8c0f40462de6e6bbcbcf1d3673`.
