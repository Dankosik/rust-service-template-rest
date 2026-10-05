# T1 — Preserve the outbound execution end

Outcome:
Replace the remaining-duration-to-relative-timeout conversion with one end
fixed at execution entry. Admission, setup, dispatch and complete buffered
collection consume that same end. At or beyond it, no new transport dispatch
or successful operation decision is allowed; the existing Timeout outcome
remains canonical. Make the decision after the last await and full buffering,
before terminal observation; report it once and return that same fixed result.

Consumes:
- [Specification: outbound deadline](../spec.md#outbound-deadline).
- [Design: fixed outbound end](../design/technical-design.md#fixed-outbound-end).
- [Outbound owner](../../../crates/infra-outbound-http/src/lib.rs) and
  [current guide](../../../docs/outbound-http.md).

Provides:
- Fixed absolute-end execution and consistent outbound consumer guidance.

Boundary:
Keep entry-time checked end selection, timeout_at and pre-transport refusal
inside existing execution ownership. After the exchange await and full buffered
collection, convert otherwise successful completion to Timeout at or beyond the
fixed end, preserving existing error classifications. Fix that result before
Attempt::finish, record it once and return it without a post-observation
deadline recheck. Synchronous terminal callbacks may delay physical return;
they cannot reopen the decision or dispatch.

Preserve buffered body limits, cancellation ambiguity, existing observation
labels and the observer's duration sample boundary. That sample can include
intervening observation work and exceed the work budget; it is neither the
operation-decision timestamp nor total physical-return latency. Update the
outdated relative-timeout description in the existing decision document. No
retry, transport wrapper, deferred reporting task, new metric policy, budget
or config change.

Mutable owners:
- `crates/infra-outbound-http` execution and its relevant tests/rustdoc;
  manifests remain unchanged.
- `docs/outbound-http.md` and `docs/outbound-http-decisions.md` time contract.
- This packet's implementation details and chosen final-validation commands.

Exclusive locks:
- none.

Final validation:
- Claim: The one fixed end cannot restart across suspension and prevents late
  transport dispatch or late successful operation decisions. Terminal
  observation reports the same fixed result once, and that result is returned.
- Checks: The ledger's consolidated matching build and relevant tests;
  documentation consistency. No additional runtime requirement. The Lead
  chooses concrete tests and records commands for the delivery owner.
- Observable: The post-await, fully buffered operation decision before
  terminal reporting, and its matching reported/returned result. Callback
  latency may delay physical return; the unchanged duration metric does not
  prove the decision deadline. No remote-effect rollback is claimed.

Reopen if:
System Design if the buffered execution owner cannot enforce the accepted end;
Specification if a changed timeout, retry or caller-visible contract is needed.


## Implementation result — Acceptance Result V1

Reconciled with the reviewed T1 operation-decision boundary. The functional
absolute-end and post-await guards remain unchanged; rustdoc, consumer guidance
and comments now state the fixed-result terminal-observation contract.

```text
unit: T1
verdict: Implemented
candidate: bounded working-tree diff in crates/infra-outbound-http/src/{lib,tests}.rs and docs/outbound-http{,-decisions}.md
provides: fixed checked execution end, timeout_at, predispatch refusal, late-ready operation-decision refusal, fixed-result observation contract, deterministic regression cases and consumer guidance; unverified
next_owner: LEDGER_ORCHESTRATOR for assembled final validation and review
```

Implementation uses checked entry-plus-operation arithmetic, retaining the
parent for an unrepresentable operation end. Both timeout guards keep the
canonical Timeout outcome and the existing attempt-completion owner. Body
collection, errors and provider-effect ambiguity stay in the existing path.

Chosen test coverage:
- `setup_spends_the_fixed_parent_or_operation_end_before_dispatch` advances
  paused Tokio time inside the existing tracing setup extension point. Parent
  first, operation first and an overflowing operation end all refuse at the
  selected end without starting a connection. The resolved Tokio 1.53.1
  `advance` implementation applies the clock change on its first poll before
  yielding; no production clock hook is added.
- `ready_response_after_the_fixed_end_is_timeout` uses the actual TLS fixture
  and the response future's wake signal to hold execution unpolled until a
  zero-length response is ready, then explicitly advances the paused clock
  beyond its fixed end. Late buffered completion must return Timeout.
- `terminal_observation_preserves_the_timely_operation_result_once` advances
  paused Tokio time in the supported terminal tracing callback, after a timely
  complete response. It asserts physical return after the end with the same
  successful operation result, one response metric sample and one matching
  span outcome. Existing observation tests do not distinguish a later deadline
  recheck that would contradict already emitted telemetry; no production seam
  or changed observation policy is needed. The completed-response fixture's
  remaining TLS teardown is stopped and joined before running its own timer
  against the artificial clock jump.
- Existing tests remain the owners of already-expired admission, complete
  body limits, pending-body timeout observation and caller cancellation.

Final commands selected for the delivery owner's consolidated run:
`make build`, `make test-package PKG=infra-outbound-http` (or the assembled
`make test-changed` invocation containing this crate), and `make docs-check`.
No checks ran during this reconciliation. Earlier final-validation evidence
is owned by the delivery owner; the added callback-boundary test and affected
documentation need final validation. Original-defect negative controls remain
with that owner; test authorship alone is not a behavioral proof receipt.
Only the two Rust files were formatted with the pinned rustfmt. There are no
remaining writers or child lanes. No manifest, dependency, budget, global
clock, exported seam, commit, push or infrastructure change was made.
