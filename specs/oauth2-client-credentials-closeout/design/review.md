# Technical Design review

Reviewer: fresh read-only `reviewer-agent`, native model `gpt-6-astra`, effort
`high`, task `/root/oauth_design/technical_design_review`, 2026-10-02.

candidate:

- [Mechanism](mechanism.md), reviewed draft SHA-256:
  `09fb0cb533e2195bcbb8ae6059536bca96875630f7864662b2271c36c78f7d8e`.
- [Library decision](library-decision.md), reviewed draft SHA-256:
  `be4b63bc7963651210004931e178c907afe64d247f62bee727c24e4ef37594c2`.
- [Ready amended spec](../spec.md), SHA-256:
  `bc78c10a723502a01bc40d91db174910e6fed6a9e18f1cc39e9bd1b862af3236`.
- Runtime baseline `67be869acea112af271ec8ba621cbc50ae9d36b7`.

verdict: PASS

findings: none.

## Evidence boundary and attempted falsifiers

- Failure/deadline interaction: cancellation poisoning, sliding suppression,
  and suppression erased by cutoff/401. Explicit timeout provenance,
  completion-based expiry, independent waiter deadlines and separately
  preserved failure state close those paths.
- Request-only exchange: non-reusable token reuse by coalesced waiters and
  repeated successful acquisition by one caller. Installed Moka 0.12.16
  documents initializer-only freshness; the design checks every returned
  value and stops immediately after that caller's successful initialization.
- Retention: independently checked the arithmetic. Each weight bounds header
  bytes and is at least ceil(B/C), so settled total weight at most B implies
  payload at most B and count at most C. Conservative under-retention fits
  the accepted targets.
- Production completion and shutdown: owner self-retention, detached
  completion, a reset budget after lock waiting, and shutdown depending on
  clients released only during a later phase. Separate Owner/Inner references,
  inline refresh, scheduled absolute deadlines, explicit shutdown input and
  the production composition await close those paths.
- Closed-owner compatibility: HTTP/gRPC cache bypass and refusal-order
  ambiguity. Both transport entry boundaries gate closure, including the
  synchronous gRPC reuse branch. For closed owners, elapsed deadline precedes
  Unavailable and local composition errors; active owners retain existing
  AuthorizationConflict/required-subject ordering. This matches amended spec
  section 4; no further Definition reopen is required.
- Library selection: independently checked the published
  [Huskarl client archive](https://crates.io/api/v1/crates/huskarl/0.11.4/download)
  and [core archive](https://crates.io/api/v1/crates/huskarl-core/0.10.5/download).
  Supported custom HTTP/authentication and optional crypto invalidate an
  absence-based rejection; the candidate instead identifies concrete
  assertion-clock, lifetime-admission, retry and adaptation costs.
- Current source supports the named owners and construction migration. The
  reviewer rechecked candidate hashes and confirmed runtime source has no
  diff from the baseline. No files changed or runtime/provider/CI result was
  produced by review.

This establishes design coherence and feasible proof, not implementation
correctness. The phase owner subsequently changed only the two artifact status
lines to ready and added review/transition receipts; semantic scope is unchanged.

reopen_owner: none currently.
