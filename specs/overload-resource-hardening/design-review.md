# Independent Technical Design Review

Current verdict: **PASS** after the D1 projection repair and S1 failed-body
hint repair below. Earlier reviews remain valid only for their unchanged scope.

Reviewer: `/root/overload_design/technical_design_review`, fresh read-only
collaboration reviewer, no inherited turns. Native selection:
`gpt-6-astra` / `high`. Method: Technical Design Review through shared Review.

## Initial Review Result V1

candidate: source `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, branch
`codex/overload-resource-isolation-20261005`, worktree
`/Users/daniil/.codex/worktrees/overload-resource-research/rust-service-template-rest`.
The reviewer independently verified these hashes before and after review:

| Artifact | Reviewed SHA256 |
| --- | --- |
| [Mechanism](design/mechanism.md) | `5b86e553ab07f17446580dda783e8dbb6d3fede0ac81d7af03315173160c2f5d` |
| [Ownership map](design/ownership.md) | `e99b7d5f6480b86f5994a0c56457873ec8a4de98def4e44275400467b56a81a0` |

verdict: **PASS**.

findings: None identified in the initial review. Planning later found F1 in
the D1 projection boundary; the repair and its fresh narrow review follow.

## Attempted falsifiers

- G1 capacity/layer order: attempted verifier-entry bypass, shared
  opening/terminal capacity, opening retention after headers and health/authn
  projection interference. Independent shared semaphores, scoped opening
  ownership, unchanged terminal custody and placement outside authn markers
  support the accepted contract; original deadline/rejection owners cohere.
- Original S1 end: attempted budget restart at headers, dispatch after expired
  preparation, late success/error and zero-length verification bypass. Original
  end plus before/after polling checks and common custody close the traces.
- Poll/timer/EOF/drop races: attempted double finalization, reversal of success
  after old end, lost held chunk on read cancellation and body retention after
  permit release. One locked terminal transition extracts the full resource
  bundle, while terminal-first checks preserve stable outcomes.
- Timer lifecycle: attempted cancellation before first poll, unexpected exit
  retaining custody and aborted work retaining provider resources. Pre-captured
  exit guard, Weak ownership, synchronous extraction, owned handle and explicit
  abort-versus-completion distinction close the traces.
- Ready empty frames: resolved Tokio 1.53.1 `poll_proceed`/`made_progress` and
  Smithy 1.8.1 polling support consuming cooperative budget for every Ready
  result within the stated functioning-cooperative-runtime boundary.
- Necessity/placement/projection: existing owners/dependencies support the
  mechanisms; public/generated contracts stay fixed, markers preserve
  containment, and immutable PR #248 remains collision evidence with its
  allocation changes excluded.

## Evidence and disposition

The reviewer independently inspected Definition hashes, source identity,
relevant gRPC/storage owners, dependency source, existing proving surfaces,
profile manifest, architecture boundaries and documentation targets.
[AWS documentation](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/timeouts.html)
independently confirms operation timeouts exclude returned streaming bodies.
Existing tests were inspected for proof feasibility under test-audit's value
bar, not executed. No files edited, builds/tests/benchmarks run, environments
created, CI checked or provider operations performed. PR #248 live state was
not refreshed; its accepted immutable disposition was consumed. This proves
design support, not implemented or measured behavior.

reopen_owner: None. A demonstrated custody/race/placement defect returns to
Technical Design; changed accepted behavior/resource scope returns to
Definition. Review performed no parent acceptance or phase movement.

After PASS only `Status: draft` became `Status: ready` in the two design
artifacts. This mechanical lifecycle change preserves the reviewed semantic
scope under shared Transition. [Design result](design-result.md) owns final
ready hashes and movement evidence.

## D1 projection repair: current Review Result V1

Planning finding F1 exposed an unsupported assumption: production-contract has
no profile markers, while the proposed direct optional-guide links could target
pruned files and marker insertion would require inventory registration. This
reopened only Technical Design documentation/projection custody.

Repair: production-contract remains unmarked. It records conditional resource
obligations and links at file level only to the always-retained configuration
policy and integration/runtime-lifecycle/persistence architecture owners. Those
owners already gate their optional-guide links. No direct removable-guide link,
prunable-section fragment, new D1 marker or D1 manifest entry is selected.
Accepted G1/S1 behavior and runtime ownership are unchanged.

Reviewer: `/root/overload_design/projection_design_review`, fresh read-only
context, native `gpt-6-astra/high`. A fresh narrow reviewer was selected because
F1 came from Planning, not the initial Technical Design reviewer's anchored
findings; the shared Review same-reviewer repair condition therefore did not
apply. Only the repaired boundary and invalidated proof were reviewed.

candidate: same source `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af` and worktree;
repaired D1 documents independently hash-verified before and after review:

| Artifact | Reviewed repair SHA256 |
| --- | --- |
| [Mechanism](design/mechanism.md) | `6877d14792d61f3f5bde1813c2cf7dd40bb5f7360f967be7bbb4fc85749d35fd` |
| [Ownership](design/ownership.md) | `8ceb99ea8ffe2df5f3d98d3df7f97e2d73e0bf88e7ec516fb7ae95e71b70d0f6` |

verdict: **PASS**. Findings: none surviving. F1 is closed at the Design boundary.

Attempted falsifiers and evidence:

- All four allowed target files survive source-only and profile removal lists,
  including ancestor removals. File-level links survive section pruning.
- Production-contract has zero markers and inventory entries; the repair adds
  neither. All 48 existing markers across the four target owners match their
  inventory. `template_init.py::_apply_markers` refuses unknown markers and
  requires exact source/inventory correspondence.
- Inspected indirect links to removable guides occur inside their capability
  markers; projection applies markers before unselected-file removal. No
  automatic link rewriting is needed.
- Specification D1 permits recording or linking obligations. Conditional prose
  can preserve every accepted resource-scope obligation without removable links
  or invented values; existing persistence guidance owns the reserve, shared
  readiness and fleet-accounting details.

evidence_boundary: Read-only source/manifest/parser/guide/spec inspection and
hash verification. No edits, builds, tests, projection execution, environments
or runtime verification. Unchanged G1/S1 and initial-review conclusions were
outside this rerun. This supports design feasibility only.

reopen_owner: None. A defect in this repair returns to Technical Design.
Planning retains its packet correction and readiness-recheck responsibility.
The reviewer performed no acceptance or movement. After PASS only the two
artifact lifecycle statuses changed from draft to ready; final identities are
in [Design result](design-result.md).

## S1 failed-body hint repair: current Review Result V1

Implementation evidence found that Failed's exact-zero size hint could make
Hyper HTTP/1 discard an expired Download without polling its stable error,
appearing as a clean empty successful response. This reopens only the failed
Body hint/framing contract, not terminal custody, scope or runtime ownership.

Repair: Failed returns `SizeHint::default()` (lower zero, unknown upper bound)
with `is_end_stream = false` and the existing stable error. Open/Succeeded keep
exact-length behavior; headers stay consumer-owned. The proving obligation now
requires the actual HTTP/1.1 response handoff after autonomous failure, within
existing storage proof owners and dependencies. No new file, knob, dependency,
packet input or execution graph is selected.

Reviewer: `/root/overload_design/failure_hint_design_review`, fresh read-only
context, native `gpt-6-astra/high`. Method: narrow Technical Design Review through
shared Review, with rust-errors failure semantics. The earlier reviewers'
unaffected conclusions were not rerun.

candidate: fixed repaired artifacts, hashes independently verified before and
after review:

| Artifact | Reviewed repair SHA256 |
| --- | --- |
| [Mechanism](design/mechanism.md) | `30b094b86617422006f07a137fc64e0b242af2d40f1008938328c7c550280d08` |
| [Ownership](design/ownership.md) | `6d0451a8a2ec4ce1466b04f2a406d47212915b4d9af0d269595d183ffd4c252b` |

verdict: **PASS**. Findings: none surviving.

Attempted falsifiers and evidence:

- Resolved Hyper 1.11.1 confirms the old defect: `h1/dispatch.rs:367-379`
  selects Known(0); `role.rs:935-946` chooses the zero-length encoder;
  `encode.rs:90-92` reports its EOF; `conn.rs:596-604` exits body writing;
  `dispatch.rs:392-398` drops the body unpolled. Unknown hint instead selects
  chunked framing for an ordinary HTTP/1.1 body-bearing response without
  Content-Length, then `dispatch.rs:401-407` polls and propagates the body error.
- Explicit positive Content-Length is respected with unknown hints
  (`role.rs:747-785`) and keeps polling active. Accepted S1 verifies zero-length
  EOF before returning Download. HEAD/bodyless status behavior is outside this
  correction; the adapter does not rewrite response headers.
- Default SizeHint invents no payload and preserves stable failure;
  Open/Succeeded exact lengths remain unchanged. This matches the
  [HTTP-body 1.1.0 contract](https://docs.rs/http-body/1.1.0/http_body/trait.Body.html).
- The existing storage crate's Axum HTTP/1, Reqwest and Tokio networking dev
  dependencies support the required consumer-level proof without another
  dependency or environment. Implementation still selects the concrete case.

evidence_boundary: accepted S1, fixed artifacts, resolved Hyper/HTTP-body source,
lockfile and existing proof dependencies inspected read-only. No product edits,
builds, tests, environments, runtime claims or broad implementation review.
Unchanged G1/S1/D1 decisions retain their prior review boundaries.

reopen_owner: None. A demonstrated defect in this correction returns to Technical
Design. T2 and final validation own implementation and executed consumer proof.
No acceptance or phase movement was performed by the reviewer. Lifecycle-only
draft-to-ready refresh followed PASS; [Design result](design-result.md) records
final ready identities.
