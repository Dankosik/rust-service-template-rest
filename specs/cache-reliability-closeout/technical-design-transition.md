# Technical Design transition

```text
status: ready
owner: Technical Design
result: specs/cache-reliability-closeout/design/reliability.md
review: specs/cache-reliability-closeout/technical-design-review.md (PASS)
movement_evidence: mechanism, supported-driver selection, material flows, resource/recovery arithmetic and file ownership closed; fresh independent review permits movement
reopen_owner: none
next_owner: Planning
```

Authoritative behavior remains [Specification](spec.md). The accepted mechanism
and ownership are [Design](design/reliability.md); its fresh review is
[Technical Design review](technical-design-review.md). Baseline `67be869`,
branch `codex/cache-reliability-closeout-20261002`, worktree
`/Users/daniil/.codex/worktrees/cache-reliability-closeout/rust-service-template-rest`.

Planning can use one integrated implementation unit: replace the manager and
streaming-credentials owners with one owned supervisor over canonical redis
multiplexing, carry the credential/diagnostic and documentation corrections,
then run one assembled final validation and final independent delivery review.
The decision adds only already-declared dependency edges; manifest changes
select the existing workspace build/test route. CI-owned Valkey/profile gates
remain with CI. The user's one separate PR remains required; merge and
deployment remain outside authority.

No upstream reopen is needed. Reopen Technical Design if implementation
contradicts cancellation, clone-drop, generation or recovery accounting;
Research for changed driver semantics; Specification for changed behavior.
The design explicitly includes unrestricted public command timeouts, a bounded
connected probe exchange, the driver's post-cap jitter correction, and the
ongoing PING/file-read costs. Concrete test cases remain executor-owned.

Only this phase's design and review/transition artifacts changed.
`make docs-check` passed on the fixed reviewed design (856 links, zero errors)
and the final design/review/transition set (860 links, zero errors);
`git diff --check` passed. There is no implementation, build, runtime, heavy
validation, CI or external-effect claim at this phase boundary.
