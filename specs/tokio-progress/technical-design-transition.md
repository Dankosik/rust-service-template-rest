# Technical Design transition

```text
status: ready
owner: Technical Design
result: specs/tokio-progress/design/mechanism.md; design/ownership.md; design/libraries.md
review: specs/tokio-progress/technical-design-review.md (PASS), including ownership panel PASS
movement_evidence: all mechanism, capacity, failure accounting, process lifetime and ownership decisions closed; no surviving material review finding
reopen_owner: none
next_owner: Planning
```

Continue from [Specification](spec.md), [mechanism](design/mechanism.md),
[ownership map](design/ownership.md), [library evidence](design/libraries.md),
and [independent review](technical-design-review.md). Branch
`codex/tokio-progress-20261005`, base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`,
checkout `/Users/daniil/.codex/worktrees/tokio-progress/rust-service-template-rest`.
The root retains continuation and authorized implementation/validation/push/PR;
merge and deployment remain excluded.

Selected decisions: ExactLength consumes at most 64 inner polls per wrapper
poll with a self-wakeup on budget yield. Existing formatting layers feed a
1024-record standard bounded channel and one dedicated output thread. The output
guard/status API supplies close, actual-flush result, loss and failure evidence;
each binary owns terminal records and final exit within its existing deadlines.
Stopped-writer loss includes discarded admitted backlog. No dependency/manifest,
configuration knob, executor, CPU pool or infrastructure addition is selected.

Planning can form dependency-ordered implementation units without inventing
mechanism or placement. The shared writer API and its three consumer migrations
must form a buildable assembled boundary; guidance and upload work have separate
owners but the delivered candidate needs the existing final concurrency review.
Implementation selects concrete tests and commands under the repository budget;
this design adds no test-plan approval or performance campaign.

Reopen Design only for an unsupported selected API, a concrete ownership/budget
contradiction or required mechanism refinement. Reopen Specification only if
behavior must change. There is no missing user-owned decision. This actor stops
at the reviewed Design boundary; no production behavior has yet changed.

Static evidence: final `make docs-check` passed with all design/review/transition
artifacts present (1,292 references, zero errors). `git diff --check` was clean. These establish artifact consistency only; no
build, runtime, performance or CI result is claimed.
