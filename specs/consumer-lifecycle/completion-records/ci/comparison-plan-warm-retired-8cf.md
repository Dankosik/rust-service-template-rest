# Matched native CI comparison preparation

Status: reviewable local preparation, no run dispatched and no improvement
measured. The primary `same-builder-serial-control.patch` was prepared against
`8cf53b2818ffdb55320cd5fafeb7e9a75129a3d8`; its input/output hashes and passing
pinned actionlint result are in `same-builder-serial-control.json`.
The full workflow bytes are unchanged from the actually linted 08eb control;
that result is reused without relabelling it as a new execution. Native
`git apply --check` passed on final F. `final-f-nonschedule-tree.json` records
every other tracked path/mode/blob, and `final-f-docker-context.json` records the
current explicit Docker context families. Read back the actual control commit
against those objects after root constructs it. No generic hashing framework
or permanent comparison workflow was added.

The primary temporary control runs the existing source chain followed by the
existing derived chain in `image-source`, sharing one checkout and Buildx
builder. Both mandatory immutable evidence uploads remain. `image-derived`
becomes a lightweight native relay of the actual derived upload outputs from
that successful combined job; ordinary `needs` failure/skip propagation keeps
the unchanged `image` aggregate closed on any earlier failure. The relay's
full native allocation is explicit control overhead in cost accounting. This
uses ordinary [GitHub job outputs](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/pass-job-outputs),
not a receipt parser or supplied success verdict.

Every measured proof command remains in the same order, and the source and
derived gates consume the same artifacts as the split candidate. Former
derived-job environment values move to the corresponding steps. A preflight
requires the fixed source-plus-four-graph selection. There is one image builder;
the comparison therefore includes source-to-derived local reuse and the split
strategy's extra checkout/builder/import costs.

The earlier `serial-schedule.patch` only adds a dependency edge while retaining
two builders. It is supplementary preparation and cannot establish the primary
production before/after cost or reuse claim.

Before root publishes the temporary branch or dispatches the bounded comparison:

1. Freeze the final source and verify that the patch changes only the declared
   scheduling and native output transport. Record both commits/workflow blobs and inspect the existing
   Docker context inclusion owner; `.github/workflows/ci.yml` is excluded. All
   retained source/package graphs, Dockerfile/base pins, release settings,
   initializer helpers/inventory and proof tools must match. Record unavoidable
   VCS_REF, commit-time and initialized output identity differences explicitly.
2. Check included native quota and root's unchanged template effect envelope.
   Use the existing workflow's selected source plus `1,7,47,65` in both arms.
   Require actual `changes` readback; a draft deferral or a narrower selection
   cannot count. Do not weaken required native gates to manufacture comparability.
   Both primary arms use matching native workflow_dispatch/all-surface selection,
   including the source migration rehearsal. The ordinary PR delta selects
   derived images without the source image and is separate evidence.
3. Use warm `type=gha,scope=runtime-image` imports with no experimental exports:
   the existing PR or workflow_dispatch event has that behavior. Record event,
   cache availability/entry identity, import logs/hits and concurrent-writer
   observations. Missing or changed cache evidence makes attribution inconclusive.
4. Run the arms serially under the same observed runner image/architecture and
   tool pins; split-arm source and derived jobs overlap as the treatment.
   Native failed-job reruns may retain successful earlier-attempt outputs, so
   every result must name its producing attempt and immutable upload artifact.
5. Retain native run/job/step timestamps, `changes` selections, all command
   measurement JSON/logs, initialized output/lock/OpenAPI/inventory bindings,
   image IDs, security and SBOM outputs, artifact IDs/digests and stable aggregate
   result. Archive digest and OCI image digest remain different identities.

Calculate image-path elapsed time from actual native timestamps through the
stable aggregate, plus full selected-workflow critical path and queue time.
Report generation/fetch/build/link/lifecycle/security/SBOM/upload timings and
total allocated image-lane/selected-workflow runner-seconds. Report cache
transfer/storage/eviction, temporary disk and retained evidence growth where
observed; state missing metrics explicitly. Command CPU/RSS is not Docker daemon
resource consumption.

C2 requires a shorter comparable selected-gate critical path with all selected
proof preserved and cost inside the accepted envelope. Equal/slower, noisy,
unmatched or insufficiently observed arms keep C2 open. No threshold, speedup
or native result is inferred from this patch.
