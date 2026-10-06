# CI measurement reopen review

```text
candidate: design/ci.md SHA-256 68e4d75bd3d3354633a81f4d6fc4f578820714be287ec654a864e7a659ca02d1; measurement delta against a3853f32b1fb8374b42cf1a6e75176f9bb19165e
verdict: PASS
findings: none
evidence_boundary: Fresh read-only Technical Design Review of C2 cache condition, final-candidate attribution and bounded measurement; no execution or acceptance claim
reopen_owner: none
```

Reviewer: native `/root/consumer_ci_design_reopen/ci_measurement_review`,
`reviewer-agent`, explicitly dispatched with `gpt-6-astra`, `xhigh` and
`fork_turns: none`; native state confirmed its active turn and returned this
terminal verdict. The reviewer owned no edits, dispatch, cache changes,
acceptance or movement. The fixed candidate hash remained unchanged through
review. The phase owner subsequently changed only `Status: draft` to
`Status: ready`, producing the [current CI design](ci.md) SHA-256
`998c4994f5d81822ea6ca96fd549cf4142da8fc4690a0d8ea00ecb33b646bcb9`.

## Attempted falsifiers

- **Removing warm-cache proof weakens C1–C3.** Rejected: the accepted
  [specification](../spec.md#c1-comparable-ci-measurement) requires a declared
  comparable cache condition, measured reduction and retained artifact proof.
  It does not require warm imports. The new claim names observed absence with
  normal imports enabled and leaves warm performance unmeasured.
- **API absence is treated as a cache-hit measurement.** Rejected: both supplied
  snapshots independently show 15 entries, 10,479,073,385 bytes and no
  `runtime-image` entry. The design additionally requires actual importer/stage
  logs, matched other restored caches and interference accounting. Ambiguity
  leaves C2 inconclusive.
- **Old control timing becomes repaired-candidate proof.** Rejected: native Git
  readback confirms workflow-only old-control differences, while the repaired
  source changes executed Make/verify inputs and the portable manifest. The
  design rejects a whole-workflow bridge from Docker equality and requires a
  new final matched pair.
- **Fast images or a recovered run substitute for whole-gate improvement.**
  Rejected: the design preserves producing attempts, distinguishes recovery
  from uninterrupted timing, measures completion through `required`, and
  rejects queue-only or image-only apparent wins.
- **Recovery enables unlimited experiments or weaker proof.** Rejected: old-run
  diagnostics precede new allocation; only one fresh matched pair is permitted
  within existing bounds. Failed/retried work and relay overhead count. Failed
  or materially incomparable proof stops the pair, and Completion must retire
  its warm-only preparation instructions before dispatch.

## Evidence boundary

The reviewer inspected [T3](../tasks/T3-image-ci.md), C1–C3, the fixed design
delta, [actual old-control readback](../completion-records/ci/actual-control-readback.json),
relevant Git differences, both cache snapshots, workflow cache/export settings
and existing command-log custody. Existing source and derived recorders retain
BuildKit logs; `workflow_dispatch` keeps imports enabled and image-cache exports
suppressed. [Docker's scope contract](https://docs.docker.com/build/cache/backends/gha/#scope)
and [GitHub's cache reference](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching)
support the distinction between configured scope, accessible refs and observed
restoration.

Actual imports, final native CI, timing reduction, quota and observed costs
remain Completion evidence. Review establishes none of them. No builds,
provider experiments, dispatches or external writes ran. The original broader
[Technical Design Review](../technical-design-review.md) retains its unchanged
scope; this receipt supplies the independent verdict for the reopened decision.
