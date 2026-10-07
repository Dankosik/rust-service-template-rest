# Technical Design transition

```text
status: ready
owner: Technical Design
result: specs/consumer-lifecycle/design/system.md
review: specs/consumer-lifecycle/technical-design-review.md — PASS
movement_evidence: All four behavior traces have selected mechanisms, ownership, compatibility and recovery gates, measurable proof boundaries and explicit external consequences; fresh independent review returned PASS with no findings
reopen_owner: none
next_owner: Planning
```

## Authoritative result and identity

- [Definition](definition-transition.md) retains requester meaning, accepted
  behavior and its reviewed source boundary.
- [System Design](design/system.md) selects mechanisms and their alternatives.
- [Runtime upgrade design](design/runtime-upgrades.md) owns full rendered
  baselines, native Git reconciliation, isolated preparation, explicit adoption
  and accepted-baseline custody.
- [Release and recovery](design/release-recovery.md) owns source/version pairs,
  the finite synthetic topology, ordered native operations and proposed external
  envelope.
- [CI](design/ci.md) owns two-lane scheduling, native admission/retry custody and
  the matched timing/cost comparison.
- [Ownership](design/ownership.md) fixes implementation/proof placement.
- [Fresh review](technical-design-review.md) owns PASS and attempted falsifiers.

Worktree:
`/Users/daniil/.codex/worktrees/consumer-lifecycle/rust-service-template-rest`;
branch `codex/consumer-lifecycle-20261006`; source
`2cb871895b9edd018205fc98223477e269fce2e9`. This phase changed only its five
design files and two review/transition receipts. Definition artifacts, runtime,
manifests and CI source were untouched. All task artifacts remain uncommitted.

The reviewed candidate hashes are in the review receipt. Initial promotion
changed only the five status fields from `draft` to `ready`. The bounded
implementation containment refinement described below then clarified placement
without changing reviewed mechanism/authority; current SHA-256 values are:

| File | Final hash |
| --- | --- |
| `design/system.md` | `203bd3da6e3c97ba3b6157f2fed1ff54020768cbf07c4599a691dd592d68c865` |
| `design/runtime-upgrades.md` | `e288bbef2e986c5a3670c042a307f35d4f766be2ec537c3f18a65f0faa2dc6c7` |
| `design/release-recovery.md` | `60722c8043358e7ee8a6a55b2fca329f7108493907755c853332cd4e4bf9960a` |
| `design/ci.md` | `d6ff4c7c60934cfc795d0766e531a5a738963ad32adcc1f3a21ee620e81555e0` |
| `design/ownership.md` | `980bfeb7540b99c7eb469a5fe2d63b3e74c2f34d58893e3c2e7d45e3de528b77` |

## Bounded Implementation containment clarification

T1's updater CLI and library are source-only, with its test. Retain
`docs/template-upgrade.md` in initialized consumers as their supported
operational route; it invokes an explicitly admitted template-tool checkout.
Do not remove the guide with the executable files or leave consumer-relative
links/commands pointing at removed source-only files. No new updater file joins
portable-sync ownership; already-portable carriers remain closed for consumers
that lack the new non-portable files.

T3's retained `scripts/ci/image-results.py` owns only the previously selected
small native-needs/job-result/upload-output aggregate and its bounded self-test.
It downloads no artifact, parses no proof receipts, and introduces no shape
engine or same-attempt requirement. Retention follows the generated CI caller;
portable-sync ownership is unchanged.

These explicit containment/placement facts preserve accepted behavior,
interfaces, authority and review scope. The same Design owner applied the
ownership refinement and static delta review; no phase restart or per-task
review is required. Implementation resumes T1 containment and T3 placement
under their existing owners. Actual generated-output/portable closure stays
with consolidated final validation. The scoped native docs-check attempt after
this refinement again could not execute lychee because the Docker socket was
absent; its image-input check passed. The one new relative link's target and
heading were statically checked; native link proof remains pending the existing
Docker-capability recovery.

## Continuation and dependency admission

The root dispatches fresh Planning without another technical-confirmation
round. Planning retains distinct execution outcomes for reusable upgrade
support, real local consumer preparation, native recovery, proof-preserving CI
improvement, and the actual externally published consumer cycle. A guide or
green local suite cannot close the latter observations.

The historical custody pair is fixed at `67be869…` → `2cb8718…`, with each
source's own helper/lock/toolchain and an identical source-only fixture overlay.
Successful release rollback uses corrected consumer A (`2cb` rendered baseline,
version `0.1.0`) and B (upgrade to final admitted template F, version `0.1.1`),
subject to the fixed identical-contract admission. F's exact SHA and produced
consumer/image identities are filled by execution at its fixed candidate.

Coordinator-reported dependency update on 2026-10-06:
[CI run 37490935654](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37490935654)
passed at exact `2cb8718…`, with all selected jobs and `required` successful;
gRPC was intentionally unselected.
[CodeQL 37490935709](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37490935709)
passed its selected Actions analysis and required aggregate, with Rust
intentionally unselected. Image job `112363276629` ran
15:51:13–16:21:33 UTC, **30 minutes 20 seconds**. This is a current observation,
not an attributed improvement versus the older `c3a3f18…` graph. The root is
finishing independent integration verdict and native artifact/receipt readback;
it supplies final admitted dependency identity before dependent Implementation.
No PR250 merge or equivalent external effect is claimed here.

Template PR/push and normal native CI authority already exists. Before asking
for missing consumer publication consequences, complete the design's independent
local source/artifact preparation and present its actual A/B commits, profiles,
baseline/lock hashes, compatibility comparison, runtime/resource inventory and
the concrete proposed owner/name/visibility/registry/ref/cost/retention envelope.
Do not silently make a private target public or change attestation guarantees.

## Proof boundary and remaining capability

The phase performed source/provider inspection, two bounded domain
consultations, root ownership self-review, scoped offline link checking and a
fresh independent Technical Design Review. No Rust build, initialization,
historical actor execution, backup/restore, CI dispatch, registry publication or
runtime deployment ran in this phase.

Before review, native `make docs-check` on the five design files passed:
20 total links, 16 unique, 16 OK, 4 excluded, zero errors. No trailing-whitespace
issues were found. This is relative-link/fragment proof, not external URL
validation. Status-only promotion leaves those links unchanged.

The final seven-file `make docs-check` rerun reached the successful image-input
check, then failed because `/Users/daniil/.orbstack/run/docker.sock` was absent;
lychee did not execute. No host lychee binary was available. Static readback
confirmed all 12 newly added relative links in the two receipts name existing
files, and those new links contain no fragments. The earlier five-file native
PASS remains valid for its unchanged link surface; the final aggregate native
rerun is explicitly unavailable, not a pass. Re-run that same scoped target
when the coordinator restores Docker capability; do not create a new checker
or runtime environment for this documentation-only phase.

A native reviewer dispatch initially failed with ENOSPC before returning an
actor identity. The coordinator recovered bounded disk space and the next
dispatch succeeded; that incident did not bypass review. The coordinator also
observed the Docker socket unavailable and insufficient spare space for heavy
execution. Refresh adequate disk, Docker, architecture/emulation and native
backup-tool capability before dependent local execution. Do not launch heavy
work, clear shared caches, install another runtime or select a paid host merely
to make this phase's proof green. This capability refresh does not block
Planning or invalidate the reviewed mechanism.

## Reopen conditions

Reopen Definition only for changed user behavior/scope or demonstrated
infeasibility of accepted behavior. Reopen Technical Design for a failed full
baseline recovery, material source/helper/profile drift, required ownership
change, incompatible A/B schema/handler contract, unsupported native backup
format/topology, or measured failure of the two-lane CI strategy. A new hash
alone or native failed-job rerun does not invalidate unchanged semantic proof.

The coordinator owns missing external authority and runtime capability recovery.
All three Design descendants (two consultation lanes and the independent
reviewer) have completed. No writer, probe or validation process remains active
under this phase actor.
