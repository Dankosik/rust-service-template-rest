# T4 — Actual local consumer release preparation

Outcome:
Two isolated synthetic consumer Git repositories exist with real fully
initialized source and committed history: minimal and durable. Durable A is
based on corrected `2cb`; the full F target render and authored consumer
evolution provide concrete B source inputs for the supported upgrade exercise
at Completion. A concrete receipt makes the remaining
consumer publication consequences reviewable without remote effects.

Consumes:
- [R1–R3](../spec.md#r1-a-real-derived-repository) and
  [release/recovery design](../design/release-recovery.md#local-consumer-and-version-preparation)
  — exact service identities/profile tuples, corrected A/B pair and versions.
- Integrated Implemented T1–T3 — complete template source F and updater.
  Freeze a local F revision after their writers join; its final acceptance/CI
  is later Completion evidence, not a prerequisite review gate for preparation.
- [Runtime upgrade design](../design/runtime-upgrades.md) — full render,
  retained baseline, isolated preparation and explicit acceptance.
- [Proposed envelope](../design/release-recovery.md#proposed-external-envelope)
  — concrete private owner/repository, registry/ref, account support, local
  runtime, zero incremental cash, bounded cycles and retention proposal.
- Adequate local disk/tool capacity gates actual full generation. Root owns
  available capability recovery; unavailable capacity cannot be called proof.
- Consumer content validation/review, metadata sealing and final A/B freeze
  gate Completion. Missing consumer remote authority gates repository creation,
  visibility/settings, pushes/tags and GHCR publication only.

Provides:
- Actual local `lifecycle-minimal` and `lifecycle-demo` repositories, generated
  with exact Design choices and each source's own public initializer.
- Durable `0.1.0` A source, small committed consumer-owned work and authored
  `0.1.1` evolution/version-lock delta in an isolated source-preparation branch,
  plus the full pristine F target render and initial capture/adoption inputs.
  These are actual committed source inputs, not an accepted B upgrade/release.
  Completion exercises the supported updater to produce and seal final B.
- `specs/consumer-lifecycle/consumer-preparation.md` with actual repository
  paths/commits, profile/render/baseline/lock identities, compatibility comparison,
  available runtime/resource inventory and the concrete missing-effect proposal.

Boundary:
Prepare real sources and their history; generation is implementation work,
not a per-task test run. Existing public generation may invoke its required
locked metadata/format/OpenAPI tools. Do not replace it with cheap projection.
Keep capture/adoption and B acceptance pending when their required consumer
review/validation is not yet available. Do not invoke `prepare` against an
unaccepted baseline or invent a bypass. T4 creates the actual A repository,
full target render and authored consumer evolution; the supported adoption,
upgrade and seal are exercised in the one Completion stage after all authored
sources are assembled. Its delivery owner first validates/reviews initial
baseline content and seals A capture/adoption, then applies the authored
consumer evolution and runs supported `prepare`, validates resolved B, obtains
the integrated review, and seals B before final native CI/publication. Native
Git identity changes and unchanged-content evidence are recorded explicitly.
This is execution of the implemented lifecycle for required final proof; no
code task waits for a passing receipt or a per-task independent review.
Do not fabricate acceptance metadata to make preparation appear complete.

The durable pair is corrected A (`2cb`, `0.1.0`)→B (F, `0.1.1`), with unchanged
compatible migration/handler contracts. It is not the historical negative pair.
Do not change template binary version or profile to manufacture two releases.
Do not create remote repositories, enable settings, push consumer refs, publish
images or deploy while preparing this unit. Only the root asks for the surviving
user-owned effects after Completion has produced the actual admitted A/B source
commits and concrete inventory, preserving already granted
template authority. Minimal stays local; no permanent demo application is added.

Mutable owners:
- Separately owned local synthetic consumer Git repositories at paths recorded
  before first mutation, including generated source, consumer-only edits,
  Cargo version/lock and isolated baseline/upgrade candidate objects.
- Task-local `consumer-preparation.md` and later Completion release/evidence
  receipt under the same delivery owner; never upstream spec/design changes.

Exclusive locks:
- The two named consumer repositories and their baseline/candidate histories.
- Frozen F is read-only input during generation; template repair first stops
  affected readers, then refreshes only invalidated preparation.

Final validation:
- Claim: Real minimal/durable outputs satisfy R1 and preserve consumer work
  through accepted A→B. R2/R3 require actual native published trust and observed
  distinct-digest A→B→A, with compatible persistent contracts.
- Checks: Matching consumer build and changed-surface checks in each repository,
  upgrade content review/validation before sealing, native exact-source CI/CD
  and registry verification, then the explicitly requested digest-addressed
  lifecycle/rollback under [ordered gates](../design/release-recovery.md#ordered-operational-gates).
  Implementation chooses commands. Missing authority does not delay authorized
  local final validation after assembly; it keeps only the external result open.
- Observable: Exact F/A/B provenance and preserved work; native signature,
  provenance and SBOM all bind each published digest and expected identity.
  Actual service commit/readiness/shutdown and published worker outbox operation
  are observed at A, B and retained A. Fixture consumption is reported separately.
  Record the running digest on partial failure; never call a local image release
  proof or count equal digests as rollback.

Reopen if:
F changes migration/handler compatibility, complete generation/adoption is
infeasible or published architecture cannot run in the selected local runtime:
return the smallest Design/capability owner. If user-owned repository identity or
visibility changes, regenerate before business edits/publication as Design
requires. Missing effect authority returns to root after concrete preparation.
