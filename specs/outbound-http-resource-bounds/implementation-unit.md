# One acceptance unit: outbound resource-bound closure

Outcome: Replace frame-retaining response accumulation and unbounded distinct
OAuth token attempts with the accepted finite resource guarantees, preserving
the transport, cache, authorization, deadline, and observation contracts. Ship
the assembled code, configuration, caller adjustments, proof, and truthful
documentation in one focused PR.

Consumes:

- [Intent](intent.md#desired-outcome) and [authority](intent.md#constraints):
  the complete focused PR outcome; no merge or deployment.
- [Specification](spec.md#accepted-delta), including
  [unchanged contracts](spec.md#deliberately-unchanged) and
  [proof obligations](spec.md#proof-obligations-and-handoff): observable rules.
- [Definition result](definition-result.md#review-result-v1): Specification PASS.
- [Technical Design](design/resource-bounds.md#buffered-body-ownership),
  [attempt ownership](design/resource-bounds.md#oauth-attempt-ownership),
  [configuration and ownership](design/resource-bounds.md#configuration-ownership-and-compatibility),
  and [proof boundary](design/resource-bounds.md#proof-and-handoff-boundary):
  closed mechanisms, compatibility changes, and file ownership.
- [Design result](design-result.md#review-result-v1): Technical Design PASS.

Provides: One assembled resource-bound implementation with its final
Completion evidence and separate PR locator. No intermediate lane is an
independently accepted output.

Boundary: One existing outbound/OAuth resource envelope. Incremental transport
storage and provider-attempt admission jointly establish the documented bound
on concurrent OAuth response accumulation. Configuration and direct Options
construction select that bound; gRPC/error/metric and documentation changes
complete the same observable behavior. Splitting these into acceptance units
would leave the accepted resource claim or its consumers unfinished. There is
no dependency, release sequence, or durable scheduler requiring a task ledger.

Replace the old collector and inaccurate bounds/replay claims. Keep historical
benchmarks explicitly tied to their measured revision. Preserve the scaffold's
adopter-owned composition boundary: document the configuration-to-Options
transfer, without adding an unused service integration registry. No new crate,
module, dependency, transport capability, benchmark project, or live-provider
certification belongs to this unit.

## Delta and mutable owners

| Existing owner | Complete assigned delta |
| --- | --- |
| `crates/infra-outbound-http/src/lib.rs` and `src/tests.rs` | Replace `Limited::collect().to_bytes()` with the design's bounded incremental Vec accumulation; preserve EOF, byte/error/deadline/observation semantics and add meaningful missing regression coverage. |
| `crates/infra-oauth2-client-credentials/src/lib.rs` and `src/tests.rs` | Options capacity admission, shared Inner semaphore, both fetch initializer lifetimes, earlier exchange deadline, closed `AtCapacity`, finite `capacity` observation, direct Options callers, and corresponding fixture/test changes. |
| `crates/infra-oauth2-client-credentials/src/grpc.rs` and `src/tests/grpc.rs` | Sanitized `UNAVAILABLE` mapping with typed source and no resource dispatch after capacity refusal. Keep this with the OAuth owner, or serialize it after that owner establishes the enum contract. |
| `crates/infra-oauth2-client-credentials/src/tests/keycloak.rs` | Migrate existing direct Options construction; this caller adjustment does not create a new live-provider proof requirement. Owned with the OAuth lane. |
| `crates/config/src/integrations.rs` and `crates/config/src/load.rs` | Default-32 positive u32 field, strict typed/text scalar decoding within OAuth markers, nonzero validation, existing literals, and loader boundary coverage. |
| `docs/configuration-source-policy.md`, `docs/outbound-machine-authentication.md`, `docs/outbound-machine-authentication-decisions.md` | Configuration/default and composition example, direct Options/exhaustive-match compatibility, shared acquisition lifetime/overload/cancellation behavior and finite metric outcome. |
| `docs/outbound-http.md`, `docs/outbound-http-decisions.md` | Distinguish payload, requested accumulator storage/transient growth, current-frame/parser/cache overhead, single-flight and provider concurrency; correct unsent pooled-connection replay wording and qualify historical benchmarks. |

All paths in this table are repository-relative; `src/tests.rs` in its first
row belongs to that row's crate. Production definitions are canonical; adjust
their existing callers, fixtures, and examples in the same unit. No OpenAPI or
other generated contract change follows from the accepted delta. If a missed
consumer is found, keep its mechanical adjustment with the owning lane and
update writable custody before editing; do not invent another outcome.

Exclusive locks: Each assigned file is exclusive to its writer. In particular,
the OAuth public Options/error/metric definitions and its shared test fixture
belong to one OAuth lane; the two configuration files belong to one config
lane. The Lead owns integration, the execution/Completion record, and final
candidate freeze. No shared manifest, generator, migration, or lockfile edit
is required. Integrate overlapping fixes serially.

## Implementation carrier and assembly

One fresh Acceptance-Unit Lead owns this unit through Implementation and its
assembled Completion under the current
[Implementation owner](../../docs/spec-first-workflow/phases/implementation.md).
Useful disjoint lanes are transport, OAuth with gRPC and its fixtures, config,
and documentation. They are optional implementation sub-lanes, not tasks or
acceptance stages. The Lead chooses useful parallelism and tests while coding,
establishes shared contracts before dependent edits consume them, joins writers,
and integrates their output before final validation. The ready frontier is
this entire unit; no external input currently blocks local implementation.

Final validation:

- Claim: The assembled candidate satisfies the Specification's body retention,
  byte/EOF/error behavior and finite provider admission across both grants;
  hits/coalesced callers, cancellation, deadlines, token/cache safety,
  configuration/direct construction, sanitized gRPC projection, observations,
  and documentation remain consistent with the accepted design.
- Checks: One non-overlapping final-validation plan under
  [AGENTS.md](../../AGENTS.md#validation-budget) and the current mixed-surface
  validation owner. Implementation chooses meaningful missing cases and exact
  commands, reuses adequate evidence, and records actual results with Completion.
  Static review establishes frame release and requested-storage growth bounds;
  process RSS is not a substitute. No new test matrix, fixture design approval,
  per-lane checks, or live-provider requirement is introduced by Planning.
- Observable: Complete successful response bytes and preserved failures;
  admitted actual attempts never exceed the prepared owner's capacity, with
  immediate pre-I/O refusal and correct release/caller behavior; accepted source
  configuration reaches Options; finite sanitized observations/projection;
  documentation accurately limits its claims. Proof is at the repository's
  selected local boundaries, not a production or measured-throughput claim.
- Review: One fresh independent review of the final assembled delivery is
  required by the accepted outcome and changed authorization/concurrency
  behavior. It includes static accumulator ownership/growth and the interaction
  of both grant paths with admission; resolve blocking findings in that same
  Completion. No code-task or lane review gate exists.
- Delivery: The Lead returns a fixed candidate with assembled local Completion
  evidence and exact remaining CI gates. The continuation coordinator owns the
  authorized branch push and one separate PR, applying current contribution and
  external-effect owners at that action and recording available or pending CI.
  No automatic `ALLOW_FULL`/`ALLOW_HEAVY` expansion is part of acceptance. Local
  acceptance alone is not CI, merge, or deployment evidence.

Reopen if: Technical Design's collector, strict scalar decode, admission
lifetime, or existing ownership cannot implement the fixed contract; reopen
Technical Design for that specific contradiction. Reopen Specification only
when an observable rule cannot be preserved, and Intake only when requester
meaning/authority changes. Routine test, caller, fixture, command, or coding
repair stays with Implementation.
