# Rust ownership review result V1

Review date: 2026-10-02. Method:
[Rust Ownership Review](../../../docs/spec-first-workflow/rubrics/rust-ownership-review.md)
through [shared Review](../../../docs/spec-first-workflow/shared/review.md).
Each lane used a fresh read-only `reviewer-agent`, native `gpt-6-astra` with
`high` effort and no inherited conversation. No source edits, builds or runtime
tests were performed by the lanes.

```text
candidate: source baseline 67be869acea112af271ec8ba621cbc50ae9d36b7;
  ownership.md SHA256 a8839dbacc28f318153f2ce92085da0c009d8ad9ce36198bff206c199b9406c4;
  system.md SHA256 5c2de64f80e8e99afa33685ec5d1356b596a0716ab709297e34c9a36feabd0cc;
  spec.md SHA256 dff3e736c20e1b03e7bb9a21116c9f097b126b8fa401b096402fac678b0c9943
verdict: PASS
findings: none across all three non-overlapping ownership lenses
evidence_boundary: static ownership/placement review of one fixed map and its
  system contract against current source; no implementation or runtime claim
reopen_owner: none
```

All lanes verified the candidate hashes before and after their review.

| Lane and native identity | Attempted falsifiers and evidence | Verdict |
| --- | --- | --- |
| Responsibility/execution ownership, `/root/jobs_design/ownership_flow` | Cancelled queued or in-flight completion could outlive admission; design retains shared capacity and retires entry before reply. Operator could claim success before commit or enter ordinary startup; provider/shared Tx/root split and provisional result contract close that path. Sampler could lose publisher coverage/failure ownership; existing peers and process-duty failure channel preserve them. Inspected attempt, transaction, worker run, engine and bootstrap sources. | PASS |
| Crate/module/dependency/visibility/generated containment, `/root/jobs_design/ownership_boundaries` | Provider could need private internals exposed or reverse config/worker dependencies; current crate-private JobId/kind grammar and shared Tx API suffice. Signal/loader reuse needs no widened root API. Jobs profile removal explicitly deletes command/projection/migration/JSON surfaces and retains the actual loader-only clap use. SQLx remains generated, migrations/manifest authored. | PASS |
| File cohesion/naming/declaration grouping/test placement, `/root/jobs_design/ownership_files` | Tried collapsing new provider and worker operator files; each has one current independent responsibility. No util/common/generic layer survives. Unit proof stays beside owner, provider/database and real process proof use current harnesses; new operator integration file has one distinct public recovery surface. No test-only production API or mandatory duplicate suite is prescribed. | PASS |

## Bounded post-panel clarification for Technical Design Review

After these receipts, owner source inspection confirmed that provider
`connect`'s three-second pool acquisition does not bound its following
`verify_session` or refusal cleanup. System section 4 now explicitly wraps the
whole operator connection admission in the existing five-second startup-check
ceiling and changes the arithmetic from 31 to 33 seconds. Ownership exports
`STARTUP_TIMEOUT` as an alias for that existing ceiling next to the already
specified operation alias. Current hashes are:

- system.md: `500048d0f10060c7054c32aa086bd4a6bd8a36700ce042fb627106c8221762cd`
- ownership.md: `df495fe086c277dca47f4ac37d2dd6d0ad9d3b933a6f8f7fde9bc7db4b386baa`

The responsible files, dependency direction, execution ownership, cleanup
owner, generated containment and test placement reviewed above are unchanged.
These earlier verdicts support that unchanged scope only. The fresh Technical
Design Review must explicitly assess the admission-bound/alias delta along
with system flows; this record does not claim it was part of the earlier fixed
candidate or supply the final macro-phase verdict.
