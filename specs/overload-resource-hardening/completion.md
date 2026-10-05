# Overload hardening — Completion

Status: **local Accepted; exact-head CI pending**. Delivery owner:
`/root/overload_storage`; publication and remote-result owner: `/root`.

This is the single assembled Completion result for T1/T2. The accepted
[specification](spec.md), [mechanism](design/mechanism.md) and
[ownership](design/ownership.md) remain the behavior and scope authorities.

## Delivered result

- G1 admits business gRPC openings before authentication under a separate shared
  K-bound. Authenticated calls retain their existing independent terminal bound;
  health bypass, zero opt-out, original deadline precedence and bounded rejection
  drainage remain. Ready empty rejection frames cooperate with other tasks.
- S1 carries the original GET deadline from before admission/SDK preparation
  through confirmed EOF. Shared Download custody releases provider body,
  withheld chunk, permit and observation even without consumer polling. Terminal
  results remain final; cancellation preserves the held final chunk, and timer,
  owner drop and provider-poll panic have bounded custody cleanup paths.
- Failed Download hints retain an unknown upper bound and false end-of-stream,
  so an HTTP/1.1 consumer observes the error instead of a clean empty response.
  Successful exact lengths, original metadata and collection allocation policy
  remain intact.
- D1 source comments and guides describe these lifetimes and the unchanged
  workload/fleet boundaries. No new dependency, feature, public knob, schema,
  provider interceptor, bootstrap task or unmerged-PR implementation was imported.

## Fixed candidate and acceptance

Base: `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`.
Branch: `codex/overload-resource-isolation-20261005`.

- Tracked product/documentation `git diff --binary` SHA256:
  `5db8c3d7f9c2d900d42aa205b9803acf70cefda8d79d6794e6c68643dbf2bf44`.
- Rust source fingerprint:
  `92ad16f93726722ea2fa2b85792cf4a574245701a9d2f8c303ff559c8e144547`.
  Its construction is recorded in [validation](validation.md).

The matching workspace build passed. The workspace suite passed **875 tests and
doctests, zero failures**; one unchanged Go wire-fixture test explicitly remains
CI-owned. All **32 gRPC transport** and **55 storage** tests ran and passed.
Required baseline comparisons failed for the intended behaviors, and the
restored candidate passes those cases. Formatting, documentation links and
applicable scoped quality evidence are recorded in [validation](validation.md).

Fresh independent assembled [Implementation Review](implementation-review.md)
returned **PASS**, with no surviving finding. All implementation, validation
and review participants have joined; no source writer or validation process
remains active for this delivery.

## Remaining external proof

Local acceptance does not establish CI or publication. Root must publish the
separate PR and consume its exact-head required checks, including:

- `make template-init-check`, classified CI-owned;
- `make test-integration-object-storage`, classified CI-owned;
- source-quality projections, whose native macOS attempt stopped in the existing
  Git batch pipe path before a projection verdict;
- exact-final lint and the ordinary applicable CI quality gates. Earlier scoped
  lint covers unchanged surfaces; the final test URL expression is compiled and
  runtime-proven, but its redundant queued lint rerun never executed.

No live-provider, performance, RSS/fleet-capacity, release or deployment result
is claimed. No commit, push or PR effect was performed by this Completion owner.
