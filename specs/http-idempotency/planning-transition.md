# Planning Transition Result V1

```text
status: ready
owner: Planning
result: specs/http-idempotency/tasks.md;
  specs/http-idempotency/tasks/T1-http-idempotency-profile.md
review: specs/http-idempotency/planning-review.md (CONCERNS after one bounded
  delta recheck by the same reviewer; F1 closed by repair; F2 and F3
  discharged by the owner with the reviewer's named repairs)
movement_evidence: one independently acceptable outcome, the selectable
  profile, has closed inputs, canonical-before-generated order, full companion
  coverage, pairwise disjoint mutable owners with one writer per file lock,
  and dependency timing that places Docker and d24d173 at final acceptance
  only. It has one assembled final-validation boundary: the local route that
  make plan prints, the two heavy runs and the none equality once each, and
  one final review. A fresh independent Task Review / Readiness permits
  movement.
reopen_owner: none
next_owner: Implementation. The root binds LEDGER_ORCHESTRATOR and dispatches
  one fresh T1 acceptance-unit-lead; after Implemented, the same Lead receives
  the final delivery boundary.
```

The ready [ledger](tasks.md) and [packet](tasks/T1-http-idempotency-profile.md),
with the ready inputs they consume, are the implementation authority. The
[review](planning-review.md) establishes readiness only. No user-owned
question survives.

## Carrier for Claude Code

Implementation follows [Implementation](../../docs/spec-first-workflow/phases/implementation.md),
the [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md),
and the [Claude Code adapter](../../docs/agent-harness/claude-code.md).

- **Orchestrator.** The root binds as Ledger Orchestrator (`/orchestrator`)
  and is the only writer of `tasks.md`.
- **Lead.** The root dispatches T1 through `Agent` with
  `subagent_type: "acceptance-unit-lead"` and the Opus model field. The unit
  carries protected risks: data integrity, concurrency safety, and a
  forward-only migration. The Lead works in the existing checkout, with no
  worktree, and the root records its returned identity in `Execution`.
- **Lanes.** The Lead implements directly or through `worker-agent` lanes, up
  to nesting depth 3. Lanes run in parallel only over the packet's pairwise
  disjoint mutable owners, and each file-level lock has one writer. Cargo,
  initializer, and Docker commands never overlap on this 10-CPU, 16 GB
  workstation, and every Cargo command in the checkout shares one target
  directory. Closed, mechanical lanes such as documentation or profile
  inventory can use the cheaper model; lanes with interacting invariants, such
  as the store and the executor seam, keep Opus.
- **Handover to final validation.** After the Lead returns `Implemented`
  through [Acceptance Result V1](../../docs/spec-first-workflow/interfaces/acceptance-result-v1.md)
  and every writer has joined, the root checks T1. It then assigns the same
  Lead the final delivery boundary with `SendMessage` to that identity. If
  that identity is lost, a fresh Lead takes the boundary from the ledger,
  the packet, and the custody below.
- **Final review.** The delivery owner dispatches the review alongside
  validation: one fresh `reviewer-agent` on Opus, bound to
  [Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md#final-delivery),
  read-only, and running no Cargo, initializer, or Docker work. Any agent that
  may make a network request receives the ledger's no-personal-data rule.
- **Completion.** The Lead returns the Completion result. The root records it
  without repeating validation, marks the ledger `done` only after `Accepted`,
  and then applies [Cleanup](../../docs/spec-first-workflow/shared/cleanup.md).

## Ready frontier

T1 is the only unit and is ready now. No dependency holds implementation.
Docker and the local baseline `d24d173` gate final acceptance only.

Inside T1, these writable scopes are pairwise disjoint and can take parallel
lanes:

- the record store and migration set;
- the inbound seam and catalog;
- the configuration;
- the composition root with the regenerated contract;
- the database proof;
- the profile machinery with the inventory;
- validation and delivery routing;
- the documentation.

The manifest set and the one lockfile regeneration form a single serial owner
and come first. Three steps follow when their inputs are ready:

- the Lead reconciles the marker regions with the inventory after the lanes
  join, before any initializer run;
- the composition-root owner regenerates the contract after the seam's
  `openapi.rs` settles;
- the routing owner admits every new untracked path to the matrix candidate.

## Final-validation boundary

The delivery owner is T1's Lead, reassigned by the root. It starts only
after T1 is `Implemented` and integrated, and it validates one fixed
candidate: `HEAD` plus the working-tree manifest recorded in custody, with
nothing committed.

It runs every local step of the route that `make plan` prints for that
candidate, once each, one by one, cheap and CPU steps before the heavy runs.
At planning time the route had 18 local steps, including `make fmt-check`,
the workspace `make lint`, `make build`, and `make test`. It adds:

- `ALLOW_HEAVY=1 make test-integration-db`, once;
- `ALLOW_FULL=1 make template-init-check`, once: 128 projections and 16
  graphs, each built and tested once;
- the one-shot `none` equality against `d24d173`, which shares the matrix's
  single `CARGO_TARGET_DIR`;
- `make secret-scan`;
- the runner's `--self-test`;
- the final review.

Nothing else runs:

- no `make verify`, `ALLOW_FULL=1 make check`, or other aggregate;
- no build per harness;
- no image step locally. The runtime image build, `make migration-validate`,
  `make container-security`, and every CI result stay pending CI and are
  not claimed.

A repair reruns only the scope it invalidates. The packet's Final validation
section is the authority for claims and observables.

## Custody

Recoverable execution state lives under `.git/claude/http-idempotency/`:

- `implementation/` holds the lockfile regeneration record and any retained
  coding-feedback record;
- `delivery/` holds the fixed candidate manifest and fingerprint, the plan
  with per-check state, a log and a result record for every check, the
  locators and SHA-256 of the runner's receipts under
  `.git/codex/template-init/`, the `none`-equality record, the final review
  record, and the Completion result.

Planning created none of this; Implementation creates it at first use. The
ledger's `Execution` and `Resume` fields point there across restarts.

## Identities and stop

SHA-256 of the planning artifacts:

- `tasks.md`: `2888849b1f788e43d360ec2093df751d63c254a26b949e5519bc344cc4a4e741`
- `tasks/T1-http-idempotency-profile.md`: `17f39541cd98f97513f6e7c97a8250f55926a569ee335f5b9e21cfb6a48cc47d`
- `planning-review.md`: `7dda38e9f0de86e08940bb1ea21220fe8bcf804d0799ade5256fc54c403e97a8`

The ten consumed inputs keep the identities recorded in their transitions.
After the review, the ledger changed only its `status` line, from `draft` to
`ready`.

Authority stays with local stage-10.3 delivery. PR, push, merge, publication,
deployment, and other stage-10 capabilities remain out of scope. The roadmap's
10.3 status and the stale 10.2 status text change only as the delivery
owner's bookkeeping after `Accepted`. Reopen only the smallest upstream owner
named in the packet; mechanical locator, marker-id, lane, and lock changes
stay with execution. The Planning actor stops here, and the root continues
with Implementation.
