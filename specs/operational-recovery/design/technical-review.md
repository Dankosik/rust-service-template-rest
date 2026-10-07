# Technical Design Review: operational recovery

## Review Result V1: initial candidate

```text
candidate: system.md 574833fa0513e68bb3f532d4c751f6df38ffe7a1fa863df181bc269a8308c48f; execution.md 6139c32018f983623c71580b877d97c97af5bd0acfd28b78615f3ab1ba6a6834; ownership.md 61e871c30041337b81817bd936ce1199f5ec898d583cbbc3fc13bc3c12f1a6c2
verdict: FAIL
findings: TD-1 missing resource-completion mechanism and required input
evidence_boundary: read-only Technical Design Review of R1-R4 flows, alternatives, coherence, required inputs, proof feasibility and delivery closure; matching Rust Ownership panel receipts consumed
reopen_owner: System Design E1
```

Reviewer: `/root/operational_continuation/technical_design/technical_review`,
fresh no-history `reviewer-agent`, native `gpt-6-astra` / `xhigh` dispatch accepted
and lifecycle observed running/completed. All candidate hashes matched before
and after review. No source edits, builds or runtime probes occurred.

TD-1 anchored the original execution.md's assertion that existing Docker/Compose
cleanup supplies daemon-work terminal evidence. Source falsified that input:
`scripts/lib/compose-postgres.sh::compose_postgres_down` suppresses `compose down`
failure and `scripts/ci/runtime-image-check.sh::cleanup` suppresses `docker rm`
failure. The shell/group could finish while resources survived, without a
generation-bound acknowledgement. The original ownership map did not select the
required owner change. This was a mechanism/input gap, not an absent test case.

Other selected traces survived: R1/R2/R3 and the accepted narrower legacy
compatibility boundary, native Cargo wrapper semantics, pinned sccache local
configuration and foreground-server support. This initial FAIL grants no phase
movement.

## Repair disposition

[Execution E1](execution.md#existing-daemon-work-completion-seam) now fixes the
pending-before-effect/terminal-after-native-readback seam in the same generation
metadata. It names the finite current Docker/Compose runner owners, their
completion inputs, failure/unknown disposition and no-pass/no-release boundary.
[Ownership D](ownership.md#responsibilities) assigns those existing script changes.
No generic cleanup daemon, discovery service, resource plugin API, new runner,
numeric runtime policy or new validation gate is introduced.

The corrected candidate introduces the missing internal acknowledgement
interface, so shared Review selects one fresh Technical Design reviewer instead
of stretching the original review's bounded-delta allowance. Rust ownership
verdicts retain only their unchanged scope as recorded in
[the panel result](ownership-review.md#scoped-identity-refresh-after-td-1-repair).

## Review Result V1: repaired candidate

```text
candidate: system.md 574833fa0513e68bb3f532d4c751f6df38ffe7a1fa863df181bc269a8308c48f; execution.md 268178dd4d7235fc97b4169617c5ccaabe4a4420f24fbd1854fbb595f5a4be1f; ownership.md 779b7db3931df36ead549bf6e6ab3b27b329d6f798b4657961b3b4993587320e
verdict: PASS
findings: none surviving; TD-1 closed
evidence_boundary: fresh read-only Technical Design Review of Definition, all three design artifacts, matching ownership receipts, initial finding/repair, current lock/verifier/Make/resource-runner source and relevant validation owners
reopen_owner: none
```

Reviewer: `/root/operational_continuation/technical_design/technical_review_repaired`,
fresh no-history `reviewer-agent`, native `gpt-6-astra` / `xhigh` dispatch accepted
and lifecycle observed running/completed. All three candidate hashes matched
before and after review. Native status exposes lifecycle, not a separate
effective-model field.

Attempted falsifiers found no surviving defect:

- TD-1's required input remains absent: the repaired pending-before-admission
  and terminal-after-native-readback seam names its current owners and retains
  unknown custody; suppressed cleanup failure is now a required change.
- Shell exit, unreachable daemon or stale acknowledgement enables release/pass:
  process completion and all matching terminal tickets are required, a wrong
  generation cannot retire a successor, and incomplete custody writes no pass.
- Failed validation becomes a pass after successful cleanup: validation result
  and custody result remain distinct.
- A new resource manager or unowned execution path is required: the finite
  source inventory remains with existing owners; internal ticket recording
  grants no cleanup authority.
- Legacy compatibility overclaims safety or permanently fences other work:
  normal release permits legacy progress, the old-versus-old race is excluded,
  and contested shared activation stays with the delivery owner.
- Script repair changes Rust ownership or runtime semantics: the retained panel
  covers unchanged Rust scope; R1-R3, E2-E3 and delivery closure remain coherent.

The reviewer checked native resource naming/removal/build evidence against
official Docker run/removal, Compose down and Buildx documentation linked by E1.
No implementation, runtime, shared-lock activation or CI pass was claimed.
No files changed or validation commands ran. The phase owner may consume this
PASS through Transition; the reviewer did not perform movement or acceptance.
