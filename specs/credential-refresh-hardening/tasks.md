# Credential refresh hardening delivery

status: ready

Completion: One separate PR contains the implemented bounded NATS, OAuth and
JWKS schedules and accurate canonical rotation/revocation guidance from the
[Specification](spec.md), with matching local validation and the assembled
independent delivery review passed. Commit, push, PR identity and selected CI
results are recorded separately from local proof. No merge or deployment is
part of Completion.

Global constraints: [Intent](intent.md), [Technical Design D1-D4](design/technical-design.md),
[Implementation](../../docs/spec-first-workflow/phases/implementation.md), and
[Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md)
remain authoritative. Preserve baseline OAuth/Redis protection, template
markers/profile pruning, existing CI gates, and unrelated work. Test authoring
belongs to each executor; checks and review run once at assembled Completion,
not per task. No actual secrets, live configuration, infrastructure or provider
operations are authorized. The current root becomes the sole
`LEDGER_ORCHESTRATOR` ledger writer after ready Planning; fresh general-purpose
Acceptance-Unit Leads implement packets in the shared worktree with disjoint
writers. Assign one existing Lead as final delivery owner after all writers join.

## Tasks

- [x] T1: NATS reconnect has bounded per-attempt spread and an accurate credential/reconnect contract.
  - Depends on: none.
  - Provides: NATS policy using the existing SDK RNG interface, associated tests and canonical messaging guide.
  - Packet: [T1](tasks/T1-nats-reconnect.md)
  - Execution: `/root/credential_nats`, Implemented and joined after the bounded [D1/D3 recovery](design/design-dependency-transition.md); locally Accepted under the assembled Completion receipt. Source SHA256 `52a1fef9a3746e1e367b51f30538552aec2a971868e71bc5e9e6ea6ffd32ac17`, guide SHA256 `84afae9e6c44aed759c922cd04703f9878ff822f7c3eaf7082ec32d6ba9f3047`, independently checked. Locked offline Cargo metadata passed; manifest and lockfile equal the base.
- [x] T2: Reusable OAuth service tokens retain their owned lifetime with bounded refresh and retry spread.
  - Depends on: none.
  - Provides: OAuth admission/queue/completion schedule policy, associated tests and canonical OAuth guides.
  - Packet: [T2](tasks/T2-oauth-refresh.md)
  - Execution: `/root/credential_oauth`, Implemented and joined; locally Accepted under the assembled Completion receipt. Four returned source/test/guide hashes were independently checked during routing.
- [x] T3: JWKS periodic refresh uses independently sampled periods while preserving worker and key authority.
  - Depends on: none.
  - Provides: JWKS deadline policy, associated tests and canonical authentication guide.
  - Packet: [T3](tasks/T3-jwks-periods.md)
  - Execution: `/root/credential_jwks`, Implemented and joined; locally Accepted under the assembled Completion receipt. Source SHA256 `d476f0bf284e09fc1cc47b90a067d73fbd8c848df34fc54779a8dc429520fc50`, guide SHA256 `baa7bdf4678fbbf7224c42449bc3f4e70599169e1c9f62e2e487c3a48c0202ba`, independently checked during routing.
- [x] T4: Cross-provider rotation guidance accurately separates publication, admission, expiry and revocation.
  - Depends on: T1, T2, T3 implemented source and provider-guide outputs; implementation dependency for final canonical summaries, not their passing proof.
  - Provides: Canonical configuration/navigation, persistence/cache/TLS/session guidance and consistent architecture summaries.
  - Packet: [T4](tasks/T4-rotation-guidance.md)
  - Execution: `/root/credential_guidance`, Implemented and joined; locally Accepted under the assembled Completion receipt. Six canonical-guide hashes and the PostgreSQL comment-only file hash were independently checked. Integration/lifecycle summaries needed no edit. The same Lead is assigned the assembled final delivery boundary below.

## Completion evidence

Implementation routing owner: `/root` as `LEDGER_ORCHESTRATOR`. T1-T3 were
dispatched to the named fresh Leads; T4 waits only for their Implemented outputs.
T1-T4 are Implemented and assembled, with all writers joined. No dependency,
manifest or lockfile delta remains. `/root/credential_guidance` is the sole
final delivery owner for consolidated validation, one assembled independent
review and bounded repair routing. Changes in several crates
select one `make build` and one `make test` under the repository's ordinary
local criterion, followed by `make docs-check` and static source/guide
consistency. Concrete cases and commands for new behavior remain executor-owned.
Use current [Validation Routing](../../docs/validation-routing.md) and
[Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md)
to consolidate evidence, retaining CI-owned dependency/profile/security gates
in the actual PR run rather than multiplying local builds. No aggregate full
check, profile matrix, database execution, cloud/load/rotation exercise or new
runner is required by this plan. One fresh assembled Implementation Review
covers changed scheduling, expiry, cancellation, coalescing, authorization and
cross-unit interactions. Resolve actual in-scope defects; rerun only invalidated
proof after repairs. Final owner records exact exercised scope and unverified
runtime claims, then the Orchestrator records Completion without repeating it.

Local Completion is Accepted for candidate
`fa371d91c30682dd6f19bb9b392c708129f8786ff7a37c6d94029c7d531c4da3`:
build PASS, workspace tests 869 passed / 0 failed / 1 CI-owned fixture ignored,
docs-check zero errors, and assembled independent review PASS. The root
verified the returned [completion](completion.md) and
[review](implementation-review.md) receipt identities and consumes their
verdict without rerunning proof. All writers, checks and reviewer are joined.
Publication and actual CI readback remain root-owned; no merge or deployment.
