# Technical Design result

## T1 operation-decision boundary reopen

Status: ready; fresh narrow Technical Design review passed.

The reviewed [Definition delta](definition-result.md#t1-reopen) replaces the
agent-added physical-return promise with the fixed operation-decision boundary.
[Fixed outbound end](design/technical-design.md#fixed-outbound-end) now makes
that decision after the exchange await/full buffering and before terminal
observation. `Attempt::finish` records that fixed result once, and execution
returns the same result without a post-observation deadline recheck.
Synchronous callbacks may delay physical return. The existing histogram sample
remains after span recording/histogram setup and before its record callback;
it may exceed the work budget and is neither decision-time nor return-latency
proof. No observer implementation change, new metric policy, task or framework
is required by this mechanism. T2–T4 and their review evidence are unchanged.

### T1 delta Review Result V1

- candidate: [technical design](design/technical-design.md), reviewed SHA256
  `2a0ae04d54dfbc647ac0ca2f1e79db65c2a7fbc7567f302f5e88be167532d469`,
  T1 boundary and related proof wording only; accepted specification SHA256
  `b91b23e84cac848d601d6739a9e41ce3ea1081bb93586b8e6a0a0d9b9a56b8e0`.
- reviewer: fresh native `reviewer-agent`,
  `/root/time_fix_design/t1_boundary_review`, Astra/high, no inherited history.
- verdict: PASS.
- findings: none.
- evidence_boundary: independently compared the accepted T1 Definition delta
  with current execute/exchange/Attempt source. Budget restart, late success
  decision, conflicting reported/returned results, duplicate observation,
  misleading metric sample and unnecessary-framework falsifiers did not
  survive. Static design evidence only; no runtime verification or checks ran.
  T2–T4 and previous receipts were outside this bounded review.
- reopen_owner: none.

After review, only the T1 draft status changed to ready. Current design SHA256
is `b39708d1738414053f89115ec9970f6cacb3991ac031d89eecfeda8d220240b5`;
that status-only refresh retains the reviewed semantic scope. The historical
T1 physical-return wording below is superseded by this reopen. Only the design
and this receipt were edited by this phase actor; implementation owners retain
the current code and their pending proof.

## Original Review Result V1 (retained for unchanged scope)

- candidate: baseline `5927ffbba351af2f7fb8635316bbfa4ae5b31da6` and
  [technical design](design/technical-design.md), reviewed SHA256
  `0d9523cb3e5a3f5f6744d81ca0000c61aac27350af116f2d6e53079d1185f78f`.
- reviewer: fresh native `reviewer-agent`,
  `/root/time_fix_design/technical_review`, Astra/high, no inherited history.
- verdict: PASS after one bounded repair and delta recheck.
- findings: none surviving; TD-1 closed.
- evidence_boundary: static inspection of accepted behavior, baseline source,
  and installed resolved library APIs/source. The delta recheck proved the
  design distinguishes retained evidence published to joined waiters from
  actual fresh provider evidence. Native Moka error sharing, zero-TTL fill
  completion, replacement retention, cancellation, and no stale invalidation
  support the repaired mechanism. Original unaffected review evidence covers
  fixed outbound end, clock failure, full Duration Retry-After rendering, and
  Redis signed-millisecond admission. No implementation/runtime proof claimed.
- reopen_owner: none for this fixed design.

The first reviewed candidate (`e78175aa633b0a3e5cfb65b662d0f810e0607fc25decb6bde29b05a8d6e7c373`)
failed TD-1: Moka's conditional-entry second lookup could publish retained
evidence as a waiter result without running the provider. That mechanism is
superseded. The repaired design gives positive retention and zero-retention
fill coordination separate responsibilities, with explicit `Retained` versus
`Fresh` outcomes and a calendar check for each retained consumer.

Immediately after PASS, only `Status: draft` changed to `Status: ready`, yielding
SHA256 `323a7760116c11c221e0cc22c89d1879f325142ef33030aff37493f37f5ec36a`.
That status-only refresh retained the reviewed semantic scope.

The continuation coordinator subsequently requested a bounded clarification of
fresh-result delivery for Planning. Step 4 now explicitly reapplies the existing
fresh lifetime guards at each caller's current usable Unix sample, including
joined waiters: expiry before not-before, with unchanged 30-second leeway and
equality. For example, validation at 100 with `exp=130` cannot authorize a waiter
resuming at 200. The previous wording named only clock usability and could be
read as omitting this delivery check; that wording is superseded. Rejection is
caller-local canonical invalid trust, without provider retry or invalidation.
No parsing order, public interface, temporal threshold or cache provenance
mechanism changes. The design SHA256 at that earlier handoff was
`e09a1671b7a75a658f1a411514d1387bd8132b4f7ea2a3a620f1507823906c81`.
The earlier independent PASS remains evidence for its original scope. The
fresh delta review below closes coverage for this delivery clarification.

Only this receipt and the design document were authored in this phase. No
production/test/manifest edits, builds, tests or external effects ran;
documentation link validation remains with assembled final validation.

## Fresh-delivery delta Review Result V1

- candidate: [technical design](design/technical-design.md), SHA256
  `e09a1671b7a75a658f1a411514d1387bd8132b4f7ea2a3a620f1507823906c81`,
  against the same baseline `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
- reviewer: fresh native `reviewer-agent`,
  `/root/time_fix_design/fresh_delivery_review`, Astra/high, no inherited history.
- verdict: PASS.
- findings: none surviving.
- evidence_boundary: independently checked only Step 4's fresh-delivery guard
  and affected reasoning against accepted intent/specification and unchanged
  `claims.rs`. The caller-current sample, expiry-first ordering, strict
  comparisons, saturating arithmetic and 30-second leeway match existing fresh
  lifetime checks. Provider errors precede successful-result delivery checks;
  no reparse, retry, invalidation or provenance change is introduced. The
  explicit falsifier (validation at 100, `exp=130`, waiter resumes at 200) is
  rejected as canonical invalid trust; unusable time remains unavailable trust.
  Static design evidence only; unaffected scope retains its prior review.
- reopen_owner: none.

## Transition Result V1

- status: ready.
- owner: Technical Design / System Integration Design.
- result: [technical design](design/technical-design.md).
- review: T1 boundary delta PASS, original PASS for unchanged scope, and
  fresh-delivery delta PASS above, each within its recorded boundary.
- movement_evidence: runtime mechanism, evidence provenance, error flow,
  lifetime, library/API choice and existing code ownership are closed against
  the ready specification; the introspection finding and delivery clarification
  retain their review coverage, and the T1 operation-decision boundary has fresh
  independent PASS coverage against its reviewed Definition delta.
- reopen_owner: System Design for contradicted mechanism evidence;
  Specification for changed observable core behavior; root's separate
  Definition path for the deferred webhook horizon.
- next_owner: existing Planning owner for mechanical T1 packet/receipt refresh,
  routed by the root continuation coordinator, then the existing Implementation
  owner resumes against the closed input.

Package/file ownership is mechanically forced by existing owners, so separate
Rust Ownership Design is untriggered. Planning can select the smallest unit;
Implementation owns concrete test selection, execution and final assembled
authorization-sensitive review. Webhook retention remains outside this core
and must not block its Planning handoff or acquire a value by inference.
This actor stops at the reviewed macro-phase handoff.
