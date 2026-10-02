# Technical Design result

## Transition Result V1

```text
status: ready
owner: Technical Design
result: specs/outbound-http-resource-bounds/design/resource-bounds.md
review: Review Result V1 below
movement_evidence: Fixed design passed fresh independent Technical Design Review; collector bounds, acquisition lifetime, configuration, compatibility, and existing file ownership are closed.
reopen_owner: none
next_owner: Planning
```

Selected mechanisms:

- Keep `Limited` enforcement; incrementally copy frames into one Vec with
  geometric `reserve_exact` targets capped at `L`, then transfer it into Bytes
  at EOF. Requested payload storage is at most `L`, with at most `2L` during
  relocation; allocator rounding/RSS and the current transport frame are
  explicitly separate. Consumed frame backing is released before the next poll.
- One semaphore in `Credentials`' shared `Inner`; both fetch initializers check
  their deadline and immediately admit before signing, retaining a borrowed
  permit through token parsing/admission. Cache hits/waiters consume no permit;
  failure/drop releases it. Closed `AtCapacity` maps to the existing sanitized
  gRPC availability response and one acquisition `capacity` outcome.
- `OAuthConfig` owns default 32 and strict typed/text decoding; Options repeats
  zero and target-capacity admission. The existing adopter composition boundary
  carries the value, without introducing an unused service registry.

The design's file map assigns each change to the existing outbound, OAuth,
config, gRPC, fixture, or documentation owner. Placement is mechanically forced
by those boundaries; a separate Rust Ownership Design fork is unnecessary.
Planning may group those changes into implementation units without deciding
another mechanism. Local names, focused fixture cases, and ordinary validation
selection remain mechanical implementation inputs, not blockers.

## Review Result V1

```text
candidate: base 67be869; design/resource-bounds.md SHA256 0cdf0c1df91f793a2bcd2f702c5a18bc140c0c059729ad244fd7f113025748a4; spec.md SHA256 4cce0ce58eb4329890d2afa3cb44011e1266fa7c8f35f75213ad7e4ad9d35782
verdict: PASS
findings: none
evidence_boundary: Independent read-only Technical Design Review against fixed Specification/Definition, current owner code, and resolved library sources; hashes verified unchanged. No implementation or runtime verification.
reopen_owner: none
```

Reviewer: fresh native `reviewer-agent`
`/root/outbound_design/design_review`, selected `gpt-6-astra`, `high`, no
inherited turns; completed 2026-10-02.

Attempted falsifiers closed: input-frame retention and transient growth;
admission bypass and premature release; lost caller deadlines and multiplied
observations; config-rs coercion and semaphore construction panic; missing
composition, sanitized gRPC projection, and resource dispatch after refusal.
The reviewer independently confirmed resolved Bytes ownership transfer,
Limited frame semantics, Tokio permit release, and Moka caller-owned
initialization/replacement. No finding or bounded repair remained.

## Boundary

Only the design and this transition result were added. Production files remain
untouched. No build, test, benchmark, commit, push, PR, merge, or deployment was
performed in this phase. Documentation-link validation and assembled delivery
checks belong to downstream validation. Historical measurements remain
historical. Reopen Technical Design for a mechanism/lifetime/ownership failure;
reopen Specification only when an observable rule cannot be preserved.
