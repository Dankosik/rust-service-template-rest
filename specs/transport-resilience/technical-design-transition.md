# Technical Design transition

```text
status: ready
owner: Technical Design
result: specs/transport-resilience/design/transport.md
review: specs/transport-resilience/technical-design-review.md
  (original scope PASS; fresh bounded gRPC delta PASS; no findings)
movement_evidence: The reviewed Definition clarification is mapped to the
  unchanged native gRPC mechanism, exact polling/error behavior and proving
  boundary. Fresh independent Technical Design review passed. All unaffected
  mechanism, ownership, conditional fallback and C1 evidence is retained.
reopen_owner: Technical Design for unavailable mechanism/ownership; Definition
  only if an accepted behavior must change or be excluded
next_owner: existing Implementation T2 owner through the root continuation owner
```

Workspace: `/Users/daniil/.codex/worktrees/transport-resilience/rust-service-template-rest`.
Branch: `codex/transport-resilience-20261005`.
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

Current SHA256 identities:

- Design: `9ef4309ca9ce31e4c26bf01eb0015d11c05f5e9ec040545d479e40dfd6a1d4a9`.
- Supporting mechanism research:
  `5330d3365fe0cd6b68b7588875d53551be526e6b5116333f5650106b13bdbf8e`.
- Review receipt:
  `d3b56e39ab9c7a3de7df202d33daf548fc13b7b8d4ad460a056e95647c04ff2d`.
- Accepted spec:
  `d82429e639fad6bf6cf011008653404ea1e7bd783b0fb1c7d6661b96d59f73f4`.

The target checkout's current AGENTS, architecture and workflow owners governed
this phase; Codex collaboration is the selected harness. The original Technical
Design handed off to Planning. This receipt now returns only the reviewed gRPC
delta to the existing Implementation T2 owner; other work retains its owners.

## Bounded gRPC disposition

Keep native HttpConnector through Tonic `connect_with_connector_lazy`, with
the cooperative 5 s DNS/TCP/TLS wait. Tower may leave its retained future
unpolled after the last live buffered caller cancels. Idle time does not renew
the original deadline. On resumed polling a pending expired dial may fail that
call, then a subsequent call can redial through the same Client. Tokio may
instead accept an already-ready result before polling the elapsed timer.
Neither eager idle physical cleanup nor strict late-ready rejection is promised.

The [Definition delta](definition-transition.md) expressly accepts these native
semantics and preserves caller deadlines, TLS, finality and no replay. The
design introduces no new connection task, supervisor, retry or dependency patch.
T2 retains code and test ownership. The root reconciles only affected T2 packet,
documentation and evidence wording through their current owners before handoff.

## Preserved implementation boundary

PG/NATS/S3 conditional fallback dispositions remain **adopt**, with no exclusion.
PG bare IPv6 and native TCP selection, auth's 2 s connect / 3 s total, NATS's
bounded server attempts and forced native termination, TLS-first/discovery
matrix, Smithy TCP-timeout propagation, and their source/profile/Docker/
classifier/retirement custody remain unchanged. The C1 repair preserves native
Subscriber ownership. Existing finality, trust, lifecycle and deliberately
unchanged surfaces remain authoritative.

Concrete test cases and commands remain executor choices, assembled once under
final Implementation validation. No new environment or duplicate transport
harness is introduced by this clarification.

## Evidence boundary

The original phase produced four task-local Markdown files. This bounded reopen
edited those existing design/research/review/transition artifacts only; the
Definition actor separately owns its spec/review/transition edits, and current
implementation edits belong to their existing actors. Source inspection and
fresh review establish the mechanism clarification, not runtime correctness.
Static local-link and whitespace checks passed. This actor ran no build, test,
container, database or live-provider proof and made no code, configuration,
dependency, commit or remote change.

`make docs-check`, regression execution and assembled validation remain with
Implementation. Async timeout/drop claims concern owned transport work under
the stated polling conditions, not cancellation of an already-started OS
resolver/trust-store call. The authorized separate PR remains pending;
merge/deployment remain outside the request.
