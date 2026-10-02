# OAuth closeout implementation unit

Status: ready; [fresh Task Review / Readiness](planning-review.md) passed.

## Outcome and boundary

Deliver one coherent repair of the existing outbound OAuth profile: the
[ready behavior](spec.md) and [selected mechanism](design/mechanism.md) become
true in the adapter, its HTTP/gRPC bindings, existing tests and supported
composition guidance. Preserve the unchanged contract listed in the spec and
the [retained-library decision](design/library-decision.md). The final result
is locally accepted code and accurate guidance, followed by the authorized
branch publication and one PR through the root continuation owner.

This is one fixed implementation unit, not a task ledger. Its changes share
credential state, token admission and construction/lifecycle ownership; splitting
the runtime, callers, fixtures or docs would leave an incomplete public source
migration or an unaccepted interaction. The individual findings are deltas of
this one profile outcome, not separate deliverables. Test writing belongs in
this unit; check execution and final review are its final validation boundary.

## Closed inputs and execution custody

- [Intent](intent.md), [Definition transition](definition-transition.md), and
  [Technical Design transition](technical-design-transition.md) carry authority
  and their fresh PASS reviews. No behavior or mechanism choice remains open.
- Working checkout:
  `/Users/daniil/.codex/worktrees/oauth2-client-credentials-closeout/rust-service-template-rest`;
  branch `codex/oauth2-client-credentials-closeout-20261002`.
  Runtime baseline `67be869acea112af271ec8ba621cbc50ae9d36b7`;
  current HEAD at planning `e9e57b3a11b2fcab75b8533472ecee4d453c84e6`.
  The amended ready Definition and Design files are current working-tree inputs;
  preserve them and unrelated edits rather than reconstructing from HEAD alone.
  The root is serially integrating upstream `546a381` (already accepted Rust
  1.99.0 pins and mechanical assertion lint edits) before implementation;
  that changes the base locator, not the OAuth behavior/design. The Lead must
  use the resulting current candidate and pinned toolchain.
- The existing root continues the request and owns publication to existing
  draft PR #219. It dispatches one fresh Acceptance-Unit Lead using
  [Implementation](../../docs/spec-first-workflow/phases/implementation.md) and
  the [Codex carrier](../../docs/agent-harness/codex.md). The Lead owns this
  entire fixed unit and its assembled local delivery, without a ledger or
  separate user-visible chat. Any internal lanes must have disjoint writers;
  the Lead joins them before final validation.
- Runtime/source/docs repair, non-destructive local validation, branch push and
  one PR are authorized. Merge, deployment, provider/platform migration and
  DPoP rollout are outside this unit. Publication remains with the root.

## Accepted deltas and file owners

All paths below are relative to the checkout. The cited spec/design own the
details; this reconciliation neither replaces them nor adds product policy.

| Accepted obligation | Current-to-target delta and writable owner | Final observable |
| --- | --- | --- |
| Spec 1: service acquisition failure sharing | In `crates/infra-oauth2-client-credentials/src/lib.rs`, preserve one acquisition mutex and add the designed completion-based one-second closed-error record; distinguish full fetch timeout from shorter caller deadline/cancellation; preserve failure across cutoff/eligible eviction and clear on success. | Immediate failures are shared within the interval without provider amplification; each caller keeps its own deadline; reusable tokens, metrics, recovery and no-replay behavior follow the accepted precedence. |
| Spec 2: explicit lifetime in both grants | The same `src/lib.rs` owns `into_token`, reusable-token decisions and the Moka initializer/waiter path. Omitted expiry is request-only; zero, expired or unrepresentable positive expiry is invalid; remove the unchecked second-result reuse path. | Each requesting call may use its own valid non-reusable acquisition once, without a refetch loop; later callers cannot inherit indefinite reuse, and invalid expiry cannot dispatch a resource request. |
| Spec 3: count and payload retention | The same `src/lib.rs` uses Moka's supported weight `max(Bearer bytes, ceil(16 MiB / configured count))` with 16 MiB capacity. Keep the existing count option/range and subject digest identity. | Settled retention meets both best-effort targets; pressure may cause reacquisition but never invalidates a valid token for its requesting call or bypasses expiry. |
| Spec 4: refresh completion and source migration | The same `src/lib.rs` replaces unmanaged construction/spawn with public `Credentials::prepare` returning Credentials plus the non-cloneable RefreshDriver; separate Owner from Inner; implement `run(existing shutdown future)` with inline refresh, queue/pending admission, original scheduled budget and observed completion. Remove the superseded unmanaged route. | Final external-owner loss and existing shutdown terminate refresh through an actually awaited production lifetime owner; one surviving clone keeps normal ownership; no detached work or ownership cycle remains. |
| Spec 4: terminal closed-owner admission | `src/lib.rs` and `crates/infra-oauth2-client-credentials/src/grpc.rs` gate new service/subject calls before local composition refusals, cache fast paths or dispatch, including synchronous gRPC reuse. | A closed owner gives Timeout for an elapsed deadline and otherwise canonical Unavailable; active owners retain existing refusal ordering, HTTP/gRPC mappings, deadlines and no replay. |
| All changed behavior and migrated constructors | Existing `crates/infra-oauth2-client-credentials/src/tests.rs`, `src/tests/grpc.rs`, and `src/tests/keycloak.rs` own regression coverage and fixture migration. Every fixture constructing credentials retains/drives the driver and observes completion; all source consumers found by normal navigation migrate with the seam. | Existing supported consumers compile and their regression proof exercises the changed behavior and interactions; fixture cleanup establishes completion rather than just requesting cancellation. |
| Design: declared runtime feature ownership | `crates/infra-oauth2-client-credentials/Cargo.toml` explicitly declares the used normal Tokio sync/macros features already in the resolved graph. | Normal no-gRPC construction/driver use has its own declared features; no package, version, backend or toolchain migration is introduced. |
| Spec 5 and operator/source compatibility | `docs/outbound-machine-authentication.md`, `docs/outbound-machine-authentication-decisions.md`, `docs/service-to-service-authentication.md`, the OAuth paragraph of `docs/architecture/runtime-lifecycle.md`, and affected crate API docs explain the new source seam, actual driver await/shutdown placement, failure window, omitted/overflow lifetime, best-effort retention, actual assertion clock and dated library decision. | A derived-service author can construct and close the integration correctly, and documentation makes no indefinite-reuse, strict RSS-bound or library-absence claim. |

The composition guide is the production recipe owner: the template has no
concrete OAuth provider to register in bootstrap. Preserve lazy profile wiring
and template markers. The recipe must distinguish expected final-owner driver
completion from failure in a process root that otherwise rejects unexpected
background completion, and await the driver in the existing background-join
phase before dependencies are dropped. A detached recipe is not completion.

Configuration defaults/ranges, inbound auth, readiness, database schema, OpenAPI
and generated runtime artifacts require no implementation for this unit.
No new crate, module, service registry, generic supervisor, test runner or
environment is selected. No roadmap stage is added or re-scoped. Existing
profile initializer/removal rules and CI workflow/classifier owners are
consumed unchanged; preserving their markers is part of the changed sources.
Routine newly discovered same-outcome consumer edits remain with this Lead;
materially different behavior/mechanism returns to the smallest upstream owner.

## Order and dependencies

The accepted spec/design and existing owners are available before coding.
Establish the credential construction/driver seam in the adapter before its
bindings, fixtures and documentation consume it. Implement the coupled state,
lifetime and retention changes in that same unit, and write regression coverage
alongside code. This order records a real source dependency without creating
separate acceptance stages. Exact tests, fixtures, assertions and commands are
Implementation's choices under the cited behavior and repository owners.

No live authorization server, new environment or CI result is required to start
implementation. The baseline's earlier OAuth/config tests and dependency check
do not prove changed runtime behavior. Reuse any baseline evidence only where
its actual code, inputs, dependencies and scope remain equivalent.

Mutable source owners remain exclusive while edited. CPU-heavy checks are
serial across worktrees; use the existing Git-common validation lock and never
clear shared caches. The main checkout's existing target cache may be reused
where compatible. Shell PATH needs Cargo, Homebrew and `/usr/local/bin` (Docker);
these are environment locators, not new validation infrastructure.

## Final validation and delivery

When all code, tests, migration and documentation are assembled and writers
have joined, the unit's Lead owns one final local validation boundary under
[AGENTS.md](../../AGENTS.md#validation-budget),
[Validation Routing](../../docs/validation-routing.md) and the
[Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md).
Use a matching build and relevant passing tests, documentation link/static
consistency checks, and the existing manifest/dependency checks selected by
the actual changed surfaces. The planned crate manifest edit retains its
existing broader build/test routing and dependency-check obligation; do not
silently treat it as a source-only edit. Required variants include affected
normal/no-gRPC and gRPC consumers, without multiplying whole builds by every
profile or inventing a new matrix. Implementation records the concrete commands
and actual scope in its completion evidence.

Select one fresh independent final
[Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md)
because authorization and concurrency behavior change. Its fixed assembled
candidate includes the coupled failure/reuse/retention/lifecycle behavior,
HTTP/gRPC compatibility, source migration, fixture completion, profile marker
preservation and truthful docs. Resolve blocking findings; rerun only proof
invalidated by repair. No intermediate unit review or test-design gate exists.

Existing selected CI gates remain owned by the current classifier/workflows,
including OAuth integration and profile initialization/removal proof. Draft
PRs skip expensive jobs; draft-only green status cannot establish those gates.
After local acceptance the root publishes the final branch and updates the
existing PR, moves it out of draft when ready under CONTRIBUTING, and obtains
the selected final-candidate CI results. It reports CI separately from local
acceptance and never infers Keycloak, performance, RSS or deployment evidence
from mocks. No extra local heavy, full-repository or provider run is required
by this plan.

Local completion means the agreed change, matching build and relevant tests
pass, docs agree, and no in-scope defect or blocking final-review finding
remains. The root then closes the authorized PR-delivery obligation with its
actual CI evidence or an exact unresolved external gate. Merge/deployment stay
outside the outcome. A missing optional observation is not a blocker; a missing
required build/test or selected CI gate must remain explicitly unverified.

## Reopen and phase stop

Reopen Definition only for changed observable behavior or the accepted source
compatibility boundary; Technical Design for a different lifecycle, retention
or library mechanism; Planning for an invalid unit/dependency boundary.
Implementation owns ordinary coding, fixture and proof-technique choices.
Planning stops after fresh Task Review / Readiness permits this unit to move;
the existing root dispatches the Lead and retains continuation/publication.
