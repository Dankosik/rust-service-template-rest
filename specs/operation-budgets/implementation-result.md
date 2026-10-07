# Operation budgets implementation

Status: Implemented; unverified behavior. Unit T001, base
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, branch
`codex/operation-budgets-20261005`.

The Acceptance-Unit Lead owns implementation and shared graph/profile custody;
the root alone writes `tasks.md`. Final build, tests, review and delivery await
the separate Completion assignment after all writers join.

## Execution notes

- Created the neutral `operation-context` leaf with fixed origin/duration
  deadlines, child budget clamping, cancellation lineage and stop primitives.
  It owns no timer task, transport projection or retry policy.
- Native execution lanes: `transport_auth` (including its `bearer` subset),
  `cache_outbound`, `storage`, `messaging_jobs`, and `oauth`. Their mutable source
  and crate-specific documentation do not overlap. Manifests, lockfile,
  architecture, profiles and cross-crate caller integration remain Lead-owned.
- Installed `yq` 4.53.6 parses TOML but its round-trip drops inline template
  marker comments. Manifests are therefore structurally read through `yq` and
  edited surgically to preserve those source-authority markers.
- Deliberate lock maintenance used
  `env PATH=/Users/daniil/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin cargo update --workspace --offline`.
  This is the accepted lockfile edit, not a validation run. The root confirmed
  the necessary write-mode exception to `--locked`; every diagnostic and
  validation remains locked. Cargo added only the local `operation-context`
  package. A TOML comparison against the base preserved all 578 registry
  package identities, versions, sources and checksums. No lockfile hand edit or
  registry upgrade occurred.
- S3 tests use the SDK-supported interceptor API via dev-only direct edges to
  already-resolved `aws-smithy-runtime-api` 1.18.0 (`client`) and
  `aws-smithy-types` 1.8.1. Existing futures-util and Tokio runtime/macro/test
  features support its resource timer and deterministic tests. A second
  workspace-only lock update selected zero new packages. This adds no
  test-only production seam.
- Leaf test authoring protects three independent behaviors: child budgets
  spend the original parent allowance, cancellation flows only downward, and
  huge legal finite durations wait/clamp without overflow. These public
  contracts had no neutral owner coverage before C1; tests need no production
  seam. The cases are written but not executed during implementation.

## Coding diagnostics

All Cargo diagnostics below use command-local Rust 1.99 PATH, the task-owned
`target` directory, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_INCREMENTAL=0`, and
`scripts/ci/validation-lock.sh`. They compile code only and are not task gates or
behavioral proof. No test, runtime probe, lint, aggregate validation or review
has run in this phase.

- `cargo check --locked --tests -p operation-context -p infra-cache -p infra-outbound-http -p infra-messaging -p infra-jobs`: passed (73 seconds). The only diagnostic is the existing vendor SQLx deprecated `Atomic::fetch_update`; that owner stays unchanged.
- `cargo check --locked --tests -p infra-http -p infra-grpc -p infra-bearerauthn -p infra-oauth2-client-credentials -p infra-webhooks`: first found a missing `tower::Service` import in `PreparedCall::send`; the same writer repaired it. The repeat passed (3.81 seconds), including production and tests.
- `cargo check --locked --tests -p infra-oauth2-client-credentials --no-default-features`: passed (19.78 seconds), covering the HTTP-only credential variant.
- `cargo check --locked --tests -p infra-object-storage -p infra-messaging --features infra-object-storage/integration,infra-messaging/integration`: passed (33.16 seconds), including provider integration fixtures at compile time only.
- Python AST parsing of the modified projection carrier, JSON parsing of the two
  changed policies, and `bash -n scripts/ci/changed-surfaces.sh` succeeded. These
  are syntax feedback, not execution of the classifier/profile assertions.
- Owned Rust formatting used the pinned rustfmt. CodeGraph sync reports the
  task worktree index current. Registry lock identities remain unchanged after
  all deliberate graph edits.


## Assembled result

All five execution lanes and the nested bearer writer are joined; the Lead's
shared graph, profiles, documentation and caller integration are complete.
C1–C13 are implemented together. Existing callers retain standalone APIs, while
webhook delivery now passes `Job::context()` to bounded outbound HTTP. Registry
handlers carry the new context; existing external template handlers infer and
ignore that argument, so no speculative caller edits were needed. Mounted HTTP
auth fixtures already use the hardened chain.

No upstream decision was reopened. No known mechanical implementation-input gap
remains. The SDK is treated as potentially dispatched after its first poll;
cancellation while unresolved SDK credential work is in progress conservatively
retains `OutcomeUnknown`, unless the SDK returns its existing definitive
classification. No dispatch-tracking interceptor was added to production.

Final build/tests, behavior proof, static policy/profile/doc gates, independent
review, commit/push and the separate PR/CI delivery remain with the separately
assigned Completion boundary. No local behavior, live provider, CI, release,
merge or deployment claim follows from these compile-only diagnostics.

## Candidate identity

Base: `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`. Source output count: 61.

Bounded source SHA256: `17ad0c00bdf94f0a7ae81bcd1174a5c4125f6fba438e8c903042900669a2408d`.

The digest concatenates each sorted changed/untracked non-`specs/` path, NUL,
its octal permission bits, NUL, its exact bytes, NUL. The separate source outputs
include the new leaf manifest/source, HTTP context module and operation-budget
guide. Workflow artifacts are excluded so the root can update its ledger
without changing the code candidate.

```text
unit: T001
verdict: Implemented
candidate: base 78aa3a832bfb4d7e9632ce5ebbbf1680705c31af + source SHA256 17ad0c00bdf94f0a7ae81bcd1174a5c4125f6fba438e8c903042900669a2408d
provides: assembled C1–C13 code, tests, caller cleanup, documentation and portable profile custody; behavior unverified
next_owner: root Ledger Orchestrator records Implemented and assigns final Completion to this Lead
```
