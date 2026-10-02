# Definition review

Reviewer: fresh read-only `reviewer-agent`, native model `gpt-6-astra`, effort `high`, task `/root/oauth_definition/definition_review`, 2026-10-02.

candidate: Definition on baseline `67be869acea112af271ec8ba621cbc50ae9d36b7`. Independently checked SHA-256:

- intent.md: `f4f9172507d3fc38639c4f4281d2a3dbcd4b81906aeecaf379e6166796d4cd78`
- spec.md (reviewed draft): `2713a6134c71c8654e09c062048893832a678cd705b1ffa0d6f71ee665c0311d`
- research/baseline-and-libraries.md: `197889b2d65b011e672105524276411bd159a9001105235136cee65bbaff9bfc`

verdict: PASS

findings: None surviving.

## Evidence boundary and attempted falsifiers

- Failure-policy falsifier: immediate refusal, requesting-caller cancellation, waiter deadlines, and eviction during suppression. The spec distinguishes completed failures from caller cancellation, preserves reusable-token precedence, and retains suppression across cutoff/eviction.
- Lifetime falsifier: omission, overflow, zero lifetime and waiting callers. The spec distinguishes one-call use from invalid response, forbids indefinite reuse and internal refetch loops; omission is compatible with RFC 6749 section 5.1.
- Retention falsifier: a byte target rejecting valid tokens or claiming an RSS ceiling. The spec limits retention only, preserves the requesting call, requires settled convergence and explicitly qualifies best-effort capacity.
- Lifecycle falsifier: cancellation mistaken for observed completion. Inherited authority closes the candidate; no specification repair is needed. [CONTRIBUTING](../../CONTRIBUTING.md) requires joining every spawned task; [rust-tokio](../../.agents/skills/rust-tokio/SKILL.md) requires an owner that observes failure, cancellation and completion. Section 4 does not waive these obligations. Technical Design must identify a compliant completion mechanism. An abandoned handle or cancellation-only proof is insufficient, and preserving the public API never overrides completion ownership.
- Baseline acquisition, refresh, admission, exchange retention and eviction inspected through the worktree CodeGraph; existing HTTP/gRPC contract and assertion documentation compared with the deltas.
- Huskarl 0.11.4 metadata and published archive paths for both grants and core HTTP/signer modules independently confirmed. Library suitability, resolved dependencies and lifecycle mechanisms remain Technical Design-owned.

Read-only review; no builds, tests, provider integration, CI or implementation acceptance. The phase owner subsequently changed only spec status from draft to ready, a mechanical lifecycle update with unchanged semantic scope.

reopen_owner: none currently. Technical Design reopens Definition if inherited completion ownership cannot be satisfied within the accepted compatibility boundary.
