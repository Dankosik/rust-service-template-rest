# Definition transition

```text
status: ready
owner: Definition
result: specs/transport-resilience/spec.md
review: specs/transport-resilience/definition-review.md (both scopes PASS)
movement_evidence: Original Definition review remains valid for unchanged scope.
  The bounded gRPC cooperative-timeout clarification passed fresh independent
  review with no findings. Only lifecycle status changed after that review.
reopen_owner: Definition if accepted behavior or compatibility must change;
  Research only for a fact required to resolve a mechanism decision
next_owner: Technical Design
```

Workspace: `/Users/daniil/.codex/worktrees/transport-resilience/rust-service-template-rest`.
Branch: `codex/transport-resilience-20261005`.
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Current spec SHA256:
`d82429e639fad6bf6cf011008653404ea1e7bd783b0fb1c7d6661b96d59f73f4`.
Intent and research identities remain those in the review result.

The current checkout's AGENTS and workflow owners govern this Definition;
Codex is the selected harness. The root retains continuation. This receipt now
returns the bounded gRPC clarification to the existing Technical Design owner;
the rest of Implementation remains under its current owners.

## Next action

Technical Design adopts the gRPC native cooperative 5 s connection wait with
its original deadline retained across idleness. On resumed polling a pending
expired dial may give one call a transport failure before a subsequent call
redials; an already-ready result may win the native poll. Neither eager idle
cleanup nor strict late-ready rejection is required. Caller deadlines, trust,
stream lifetime and no-replay/finality remain unchanged. Update only dependent
design, planning packet and implementation proof language through their owners;
retain unaffected decisions and work. No user technical confirmation is needed.

## Evidence boundary

The original Definition created five task-local Markdown artifacts. This reopen
changed only spec.md, definition-review.md and this transition; concurrent
implementation files belong to their existing actors. Static consistency and
fresh review establish Definition readiness, not code correctness. This actor
ran no builds, tests, containers or live-provider proof and changed no code,
tests, dependencies or runtime configuration. `make docs-check` remains with
assembled final validation. The separate-PR outcome remains authorized;
deployment and merge remain outside the request.
