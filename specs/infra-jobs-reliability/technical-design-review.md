# Technical Design review

Status: ready — original PASS retained for unchanged scope; adoption delta PASS.

Source baseline: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Worktree: `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-infra-jobs-reliability-20261002`.
Authority: ready [Intent](intent.md), [Specification](spec.md), and
[Definition transition](definition-transition.md). Definition Review is reused;
this receipt does not repeat it.

## Original Rust Ownership Review (retained for unchanged scope)

The conditional panel is triggered by the changed adapter, binary and typed
configuration responsibilities plus optional-profile/generated containment.
Three fresh read-only reviewers applied the non-overlapping lenses in
[Rust Ownership Review](../../docs/spec-first-workflow/rubrics/rust-ownership-review.md).
Each was natively dispatched with `gpt-6-astra`, effort `high`, and
`fork_turns: none`; each completed without model fallback.

Fixed panel candidate SHA256 values:

| File | SHA256 |
| --- | --- |
| `design/ownership.md` | `e4c30001850b4107623cef717927ff00b4ed3249fdd36df62bd3ff3f879eed4a` |
| `design/system.md` | `35dbaebc6c8639481583fbb29cbfa73d77f35560527d17663a6b633ac7ce0dac` |
| `design/rollout.md` | `df62cfe13ab7afd78a22944afd5b9399764002f1d290c8f785e8e93398da85c7` |
| `research/mechanism-evidence.md` | `4abca480fd90ffc9ba69f53d5d27f32dbf49a1d294f035bb9ba0bf59af8bdc97` |

All reviewers verified the fixed identities. Their evidence boundary is the
design, current source and repository owners in this exact checkout; no build,
test, database, broker, generated-profile or runtime result was claimed.

| Native reviewer | Lens | Verdict | Attempted falsifiers and result |
| --- | --- | --- | --- |
| `/root/infra_jobs_design_resume/ownership_paths` | Responsibility and execution path | PASS | Queued/in-flight cancellation, retry registration and forced cleanup retain the existing supervisor/completer owner; commit finality stays with infra-postgres and recovery with infra-jobs; one maintenance owner controls retention and union sampling; operator configuration, pool/signal lifetime and exit projection have separate explicit owners. No surviving finding. |
| `/root/infra_jobs_design_resume/ownership_boundaries` | Placement, dependencies, composition, visibility and generated/manual containment | PASS | Public recovery request/result types have the actual jobs-worker consumer, with no reverse config/binary dependency; command selection precedes full startup; custody/peer/CLI helpers stay private; jobs profile markers prune modules/dependencies/migrations; schema and fixed SQL own genuinely generated SQLx metadata. No surviving finding. |
| `/root/infra_jobs_design_resume/ownership_cohesion` | File cohesion, naming, grouping and test placement | PASS | Collapsing the operator modules mixes table behavior with command lifecycle; R1–R7 reconcile with the inverse map; tests remain at config/lifecycle, CLI-only, or admitted DB boundaries without duplicate required proof or test-only production seams. The existing fixture already forwards args to jobs_worker::run. No surviving finding. |

Panel synthesis: PASS on the fixed Ownership Map; every triggered lens passed
and their boundaries are compatible. Reopen owner: none.

Before overall Design Review, the phase owner clarified only the completion
notification ordering in `design/system.md`: remove the previous completion
entry and batch SQL buffers before waking a retry waiter. A no-await interval
alone cannot serialize separate Tokio worker threads. The original design
already required entry disposal before re-registration/retry; this clarifies
its mechanism without changing R1 ownership or the accepted contract. The
path reviewer explicitly confirmed unchanged responsibility/execution ownership.
The other map/placement/cohesion reasoning is unaffected. Overall Design Review
consumes this correction and the panel receipts without repeating the panel.

## Original Technical Design Review (retained for unchanged scope)

Fresh reviewer `/root/infra_jobs_design_resume/design_review`, natively selected
`gpt-6-astra`, effort `xhigh`, `fork_turns: none`, applied
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md)
through shared [Review](../../docs/spec-first-workflow/shared/review.md).

```text
candidate: source 67be869acea112af271ec8ba621cbc50ae9d36b7;
  design/system.md d6ca60357cbeef6baccd1ab81305a9c332a6fde235f1442b36aea9ce990d00ea;
  design/ownership.md e4c30001850b4107623cef717927ff00b4ed3249fdd36df62bd3ff3f879eed4a;
  design/rollout.md df62cfe13ab7afd78a22944afd5b9399764002f1d290c8f785e8e93398da85c7;
  research/mechanism-evidence.md 4abca480fd90ffc9ba69f53d5d27f32dbf49a1d294f035bb9ba0bf59af8bdc97;
  panel receipt technical-design-review.md 74aa13f1b7d1eca1d0af32b6d9dd0e55cdbfc973568e931249d93fa60e3bb339
verdict: PASS
findings: none
evidence_boundary: fixed design and accepted Definition, current repository
  source/owners and official PostgreSQL semantics; static design verdict only
reopen_owner: none
```

The reviewer verified all identities unchanged and consumed the ownership panel
without repeating it. Attempted falsifiers and outcomes:

- Queue cancellation, in-flight writer cancellation, earliest deadline expiry
  and retry on another Tokio thread: shared custody, exact-entry removal,
  batch-held permits and disposal-before-reply support the N bound while
  preserving known-result priority and immutable deadlines.
- Concurrent recovery/discard, later-cycle stale reuse, live-key races and
  unknown commit: primary-key locking, sequence advancement, existing unique
  arbitration and acknowledged-commit-only success close each durable trace.
  Current `in_tx_with` supplies finality; official PostgreSQL
  [sequence](https://www.postgresql.org/docs/18/functions-sequence.html) and
  [READ COMMITTED](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED)
  behavior support the mechanism.
- History erasure or outbox reconstruction: the explicit update preserves
  history, prepared bytes and publication identity; cycle tags, restore
  invalidation and indefinite-replay obligations are explicit.
- Accidental full-config/provider admission, unsafe diagnostics, sparse-page
  omissions and timeout-as-empty: the existing projection/entry seam plus
  bounded PK windows and explicit cursors/outcomes close these paths.
- Partial observation freshness, late peers, old-worker deletion and unsafe
  rollback: one union sampler, membership invalidation/recheck, completed-only
  maintenance and the ordered old-owner stop/replacement sequence are coherent.
- Unavailable proof or invented infrastructure: existing jobs/process/outbox,
  transaction/migration and factored profile routes own the future evidence;
  no additional environment, SLO or aggregate gate is required.

After PASS, only the status lines in `design/system.md` and
`design/ownership.md` changed to ready; the reviewed semantic scope is unchanged.
The transition records their ready hashes. This receipt records completed
review and does not claim implementation, build/test, database, broker, CI,
deployment or acceptance evidence. The phase's separate `make docs-check`
static link check passed; tracked source stayed unchanged.


## Narrow immutable-foundation adoption review

The new evidence is available implementation at immutable PR #225 source
`a04b699f744038bf9b529bdee89eda76c00f59a6`, with the two verified mechanical
repairs at `5a683be7098fdba4981afffd774f59fed40145ed`. The existing source
mapping closed the equivalent-representation fork; it does not transfer runtime
acceptance or CI claims. Definition and the old-worker rollout gate are unchanged.

Only the adopted archive/Tx API/CLI representation, its actual cli.rs and
operator.rs placement/profile footprint, corrected 12s provenance, and D1–D6
bypass closure invalidate prior reasoning. Canonical table/config/binary
responsibilities and the three-panel findings remain compatible; the selected
review is one fresh bounded Technical Design delta review of those changed
edges. It does not repeat unaffected ownership lenses or Definition.

Fresh read-only reviewer
`/root/infra_jobs_design_resume/adoption_delta_review`, natively selected
`gpt-6-astra` at `xhigh` with no inherited turns, returned:

```text
candidate: unchanged source HEAD 67be869acea112af271ec8ba621cbc50ae9d36b7;
  immutable foundation a04b699f744038bf9b529bdee89eda76c00f59a6;
  admitted mechanical descendant 5a683be7098fdba4981afffd774f59fed40145ed;
  design/system.md 9908b527fe9882232de1e3129694a6099de2c1063783a1096dbf19816165f0db;
  design/ownership.md 406d84f5b24f2669b375dd178b8f250f19f6216970d8e7af481b48cb46c0a4b1;
  design/rollout.md 52117e720bef3264b0a9f36f70bdac45ab44d432b44c9da395ff3efee532d392;
  research/mechanism-evidence.md b439dc746b44023f11cace3318ce8da4ed244bceea36dada04bfe5ff9fce2d4a;
  technical-design-review.md e06488dae51f7e284dca3efaaadcaa6dabb509cd295968a720649497bfae2eaf
verdict: PASS
findings: none
reopen_owner: none
```

All five hashes matched before and after review. Attempted falsifiers were
history loss/cycle confusion; premature success from the provisional Tx API;
cross-thread wake before complete batch retirement; late-peer stale membership;
read-only canonical-admission bypass; mutation lock timeout bypass; missing
handled-set context on failure; CLI/profile containment or inspection-bound
expansion; and finite deduplication/restored-token assumptions. None survived
the selected adoption plus D1–D6. The reviewer checked the immutable descendant
diff and reused prior panel reasoning only where unchanged.

The review establishes design/source readiness, not that D1–D6 are implemented
or that foundation tests/CI pass for the assembled candidate. No live/dirty
parallel checkout, build, test, runtime or external effect was used. After PASS,
only system/ownership status lines changed to ready; current ready hashes are
in the transition. Existing coverage is reusable source and must be assessed
under final validation before any candidate-proof claim.
