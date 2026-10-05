# Cancellation and cleanup: fixed implementation unit

status: ready

Planning result under [Planning](../../docs/spec-first-workflow/phases/planning.md).
This is one fixed inline unit, retained here for the actor handoff; it is not
a task ledger. Read [Intent](intent.md), the accepted [Specification](spec.md),
its [independent review](spec-review.md), and the
[Definition transition](transition.md) as the upstream authority.

## Candidate and carrier

Worktree: `/Users/daniil/.codex/worktrees/cancellation-cleanup/rust-service-template-rest`.
Branch: `codex/cancellation-cleanup-20261005`.
Base: `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`.
Intent SHA256: `4f19b19de8a3a7facc0ed60f86513f2d01485c240d2a7b5de199e2655d2a2dd4`.
Specification SHA256: `8b1c7663028732a4e6da6393f7186f7fe4ef4fb2a2cb8725484ca5f017d3836f`.

Select one Acceptance-Unit Lead under
[Implementation](../../docs/spec-first-workflow/phases/implementation.md).
The continuation coordinator dispatches that fresh actor; no ledger, parallel
writers, separate check tasks or new worktree are needed. The Lead owns the
assembled result through final validation and applicable final review.

## Unit: supported cancellation and cleanup path

**Outcome.** A developer can consume the existing infrastructure through the
maintained feature entry guide without inventing cancellation machinery:
terminally failed downloads release their owned SDK body immediately, and
the supported request/job recipes describe the matching local-ownership and
remote-outcome boundaries. Deliver this outcome as one separate PR.

**Atomicity.** R1 and R2 are the implementation and developer-facing contract
of this one accepted cleanup path. Neither is a separately scheduled rollout,
provider migration or independently gated target. A code-only result leaves
the accepted discoverability and usage contract unfinished; a guide-only
result leaves its retained-download cleanup behavior false. Keep the repair,
its proof code and the guide changes in this unit and accept them together.

**Current-to-target delta.** `Download::poll_chunk` already discards the held
chunk and completes its observation/admission owner on failure, but keeps
the native `ByteStream` in the retained wrapper. Dispose of that original
body in the existing terminal failure transition before returning its error.
Preserve Specification R1 in full: repeat error, metadata, size hint,
one failure observation, admission release and existing success/empty/remaining
collection semantics. The private representation and meaningful proof code
belong to Implementation.

Expand the existing guides into the discoverable R2 recipe path: ordinary
awaited work; buffered object reads inside the handler's existing timeout;
durable follow-up work with stable business identity; and the distinction
between stopped waiting, local release and established remote outcome.
Keep provider calls behind the feature's existing business interface. Preserve
the memory/admission distinction, presigned-GET alternative, conditional
prompt-reader streaming, admitted deadline reuse where supported, and the
limits of Drop/abort/blocking work. Link the canonical provider, transaction,
jobs and HTTP-idempotency guidance instead of duplicating their contracts.

**Writable owners.**

- `crates/infra-object-storage/src/download.rs`: private state/cleanup,
  method documentation and local proof code if that is the smallest layer.
- `crates/infra-object-storage/src/tests.rs`: existing crate proof surface
  when the selected behavior proof belongs there. Reuse adequate coverage;
  no new production test API or dependency is implied.
- `docs/first-production-feature.md` and `docs/object-storage.md`: maintained
  discovery and usage recipes. Keep optional-profile content and cross-links
  inside their existing template marker conventions.
- `specs/cancellation-cleanup/`: Implementation's current result, selected
  final commands/evidence, review receipt when selected, and PR handoff.
  Intent and Specification remain upstream decision authority; do not rewrite
  them to fit an implementation.

Other code, manifests, dependency locks, profiles, generated contracts and
initializer logic have no planned delta. The executor may reconcile a
mechanical writable locator under Implementation if necessary for this same
outcome, preserving single ownership; changed behavior or scope reopens the
smallest upstream owner.

**Consumed sources and order.** Start from the fixed Specification and current
`Download` owner. The existing `ObjectStorage::get` in
`crates/infra-object-storage/src/lib.rs` supplies the body; observation is owned
by `crates/infra-object-storage/src/observe.rs`, and both chunk and HTTP-body
reads converge on `Download::poll_chunk`. These are source references, not
additional changes. Implement the repair and matching proof code, then align
the two guides with the final behavior. Routine coding order within this unit
is executor-owned.

The current [component boundaries](../../docs/architecture/boundaries.md),
[object-storage guide](../../docs/object-storage.md),
[jobs guide](../../docs/background-jobs.md),
[HTTP-idempotency guide](../../docs/http-idempotency.md),
[persistence architecture](../../docs/architecture/persistence.md) and
[outbound guide](../../docs/outbound-http.md) remain their contracts' owners.
`crates/infra-http/src/harden.rs` owns the already-present outer HTTP timeout.
The guides are canonical handwritten inputs; `scripts/lib/template_init.py`
and `scripts/lib/template_profiles.json` consume their profile markers.
There is no generated source to author or regenerate for this delta.

## Dependencies, proof and external boundary

Implementation inputs are ready: reviewed R1/R2, forced existing owners,
available source and one exclusive writable scope. Technical Design is
untriggered as established in Definition. There is no service, provider,
database or future test-environment prerequisite to coding. The parent keeps
the shared code/test validation lock closed through assembly; this Planning
result does not release it. The assigned delivery owner opens the single final
validation boundary only when all planned edits are complete and all writers
have joined, subject to shared resource availability.

Final observable: while a caller retains a terminally failed download, the
original native body is already disposed and every R1 compatibility promise
still holds; a reader of the maintained entry guide can use the complete R2
recipes without inventing a timer/task/lifecycle owner or inferring remote
rollback. Retained optional profiles keep usable recipes and valid links,
and omitted profiles keep no broken references to removed capabilities.

Implementation chooses concrete tests, fixtures, assertions and exact
commands while changing the code. Final validation consolidates the matching
build, relevant crate proof, documentation consistency/link checks and applicable
template checks under [AGENTS.md](../../AGENTS.md#validation-budget),
[Validation Routing](../../docs/validation-routing.md), and current CI routing.
No preliminary test-design approval or per-subresult execution/review gate is
created. Existing applicable CI gates remain intact; stronger runtime claims
are not acceptance requirements. Apply shared
[Review](../../docs/spec-first-workflow/shared/review.md) at the final assembled
delivery boundary, with its actual trigger and changed-outcome scope.

After local acceptance and the required external-effect preflight, commit,
push this working branch and open the requested separate PR under the existing
authorization. Record exact candidate and PR identity and applicable CI evidence;
local checks alone do not prove the PR/CI outcome. Merge, deployment, live
provider writes and production operations remain outside scope.

## Obligation closure and readiness walkthrough

R1 is owned by the Download delta and its compatibility proof. R2 is owned by
the two guide deltas, including optional-profile preservation. Every remaining
research recommendation keeps its explicit disposition and reopen condition in
[Specification](spec.md#recommendation-dispositions): plain SQLx and its
whole-return backport stay unchanged; transport timing probes, streaming
lifetime owners and broader remote-effect/shutdown experiments are conditional
and outside this PR; no helper/framework/API/dependency/configuration is added.

Written dry run: the Lead reads the accepted contract, locates the one terminal
failure transition shared by both read interfaces, changes its local body
ownership and writes behavior proof in the existing crate. The Lead updates
the entry and provider guides, following their current markers and links, then
freezes the assembled candidate for the one final validation boundary. Passing
required local proof and any triggered final review permits local acceptance;
the authorized separate PR and its CI evidence are then recorded distinctly.
No step needs a new product, architecture, provider or rollout decision.

Reopen Specification if preserving R1 proves impossible or the accepted recipes
have a demonstrated behavior gap. A newly required runtime ownership mechanism
also reopens its Technical Design owner. A missing optional transport observation
or an executor's fixture/command choice does not reopen Planning. A genuine
new independently acceptable outcome or external gate reopens Planning's unit
boundary; routine code/proof repairs remain with Implementation.
