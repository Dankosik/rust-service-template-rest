# Definition result

## T1 reopen

Status: ready; fresh narrow Specification review passed.

The continuation owner reopened only T1 after the delivery review exposed an
agent-added physical-return promise. The prior requirement that all
instrumentation fit the deadline conflicts with exactly one truthful terminal
observation when a synchronous outcome-bearing callback consumes the remaining
time. A second deadline check cannot retract the first recorded outcome;
reordering only moves the final callback. Current
[execute](../../crates/infra-outbound-http/src/lib.rs) and
[terminal observation](../../crates/infra-outbound-http/src/observe.rs) establish
that ordering. This is a bounded static argument, not a runtime measurement.

The original user outcome requires preserving time budgets through suspension;
it does not require interrupting arbitrary synchronous terminal callbacks.
Under [Evidence Contract](../../docs/spec-first-workflow/shared/evidence-contract.md),
agent-authored acceptance cannot add that requirement. The revised
[outbound contract](spec.md#outbound-deadline) keeps one fixed operation end and
requires the final success decision strictly before it, after the last await
and full buffered collection, before terminal observation. Observation records
that fixed result once; callback delay may affect physical return but cannot
restart dispatch, work, or the decision. Intent and T2–T4 are unchanged.

### T1 delta Review Result V1

- candidate: [specification](spec.md), reviewed SHA256
  `14f6e062b05b392e8cb040d8c318a772c147fdaac7789837a3865b73533b1e34`;
  unchanged intent SHA256
  `30492bd9347cdea345d49e1810ef3159c61dcd564ccb7b36dbbe5ae2a9cdc723`.
- reviewer: fresh native `reviewer-agent`,
  `/root/time_fix_definition/t1_boundary_review`, Astra/high, no inherited history.
- verdict: PASS.
- findings: none.
- evidence_boundary: T1 boundary only, independently checked against unchanged
  intent, Evidence Contract, current execute/exchange and terminal observation.
  Falsifiers for budget restart, equality/late success decisions, conflicting
  observation and return results, hidden requester-policy relaxation, and
  ambiguous decision timing did not survive. Static specification evidence only;
  other clauses retain their prior review.
- reopen_owner: none.

After review, only the specification's T1-reopen draft status changed to ready.
Its current SHA256 is
`b91b23e84cac848d601d6739a9e41ce3ea1081bb93586b8e6a0a0d9b9a56b8e0`;
this status-only refresh retains the verdict's semantic scope.

## Original Review Result V1 (retained for unchanged scope)

- candidate: source baseline `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`;
  [intent](intent.md) SHA256
  `30492bd9347cdea345d49e1810ef3159c61dcd564ccb7b36dbbe5ae2a9cdc723`;
  [specification](spec.md) reviewed SHA256
  `2b55b95864a16f71af3bd93d8da0053e1596a99e0a9f0d18d2b436e98d1ffb7c`.
- reviewer: fresh native `reviewer-agent`,
  `/root/time_fix_definition/spec_review`, Astra/high, no inherited history.
- verdict: PASS.
- findings: none.
- evidence_boundary: static comparison against baseline source and authentication
  documentation. Falsifiers covered fresh/coalesced versus cached expiry,
  pre-epoch clock refusal and recovery, fixed retention and newer replacement
  protection, late outbound dispatch/success, duration rounding and TTL bounds,
  server refusal, and already-correct OAuth overflow handling. No contradictory
  observable behavior survived. No runtime or passing-test claim.
- reopen_owner: none for the independent core.

After review, only `Status: draft` changed to `Status: ready`; specification
SHA256 is now
`7975fb9410525b49bed7b84bda2229c403277ac914f03cbc732f431bff716706`.
That status-only refresh retained the original verdict. Its T1 boundary is now
superseded by the narrow reopen above; its other semantic scope remains valid.
No production, test, manifest, or infrastructure changes were made by Definition.
Documentation link validation remains with assembled final validation, as
directed by the continuation owner; no build or test commands ran in Definition.

## Transition Result V1

- status: ready.
- owner: Definition.
- result: [intent](intent.md) and [specification](spec.md).
- review: T1 delta PASS above; original PASS retained for unchanged scope.
- movement_evidence: the fixed T1 operation-decision boundary preserves the
  requester outcome without an impossible callback-return promise; independent
  narrow review found no material divergence. No other contract reopened.
- reopen_owner: Definition if core behavior changes; Intake/Specification for
  the separately deferred webhook policy when the user supplies its horizon.
- next_owner: Technical Design's existing owner for the bounded T1 reopen,
  through the root continuation owner.

Technical Design owns the fixed-deadline mechanism, introspection cache
representation and safe replacement/coalescing, clock-failure propagation, and
cache TTL error representation. It must preserve the reviewed behavior and
leave exact test selection to Implementation. The pending user-owned webhook
receipt-retention versus delivery/replay horizon remains outside this ready
core; the root reconciles that item before final PR scope. No retention default
or retry policy may change by inference. This phase stops at this handoff.
