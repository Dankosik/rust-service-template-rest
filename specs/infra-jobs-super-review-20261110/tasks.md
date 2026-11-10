# Infra-jobs readability recommendations

status: done

Completion: All R-001 through R-033 are implemented and assembled; `make build`,
the `infra-jobs` and consumer tests, formatting, lint, documentation-link
checks, and SQLx metadata checks pass; the pull-request gate is green; the
change is merged to `main`.

Global constraints: [Specification](spec.md), [Intent](intent.md), and the
Planning Ledger Contract govern execution. Tests are written with the code and
run at the single final-validation boundary after T1 through T5 are implemented
and assembled. Only the bound Orchestrator writes this index. Every unit reports
one Acceptance Result V1; repairs return to the owning unit.

## Tasks

- [x] T1: The public job contract has truthful docs, one public policy bound
      source, named `Disposition` fields, and the validated/unvalidated kind
      names `Kinds`/`KindsBuilder`.
  - Depends on: none.
  - Provides: public paths and shared limits consumed by T2, T3, T5.
  - Packet: [T1](tasks/T1-shared-contract.md)
  - Execution: `ses_e3ae04399ffey1U1EH6srm87gT`
  - Result: Implemented; workspace check, `lint-changed` over
    `infra-jobs jobs-worker infra-messaging infra-webhooks`, `fmt-check`, and
    `test-package PKG=infra-jobs` (48/48) passed. R-029 forced caller updates in
    `infra-messaging`/`infra-webhooks`; recorded below.
- [x] T2: `attempt.rs` has no derived batch arrays, no unreachable completion
      arm, and one owner per label, wait clamp, gauge name, and log key.
  - Depends on: T1.
  - Provides: the assembled attempt lifecycle used by T3.
  - Packet: [T2](tasks/T2-attempt.md)
  - Execution: `ses_e3ad21377ffeZ8119d15bFGW9p`
  - Result: Implemented; `lint-changed PKGS="infra-jobs"`, `test-package
    PKG=infra-jobs` (51/51), doc-tests, and `rustfmt --check` passed.
- [x] T3: `claim.rs` and `engine.rs` have one lease formula, one exhaustion
      computation, one statement-turn scope, and named task/peer contracts.
  - Depends on: T1.
  - Provides: the claim/engine candidate consumed by final validation.
  - Packet: [T3](tasks/T3-claim-engine.md)
  - Execution: `ses_e3ad21374ffekzMeIsg5YpN4Xk`
  - Result: Implemented; `sqlx-prepare`/`sqlx-check`, `lint-changed`,
    `test-package` (51/51), the `execution::` real-database suite (45/45),
    `docs-check`, and `rustfmt --check` passed. The guide's lease wording now
    matches the shared offset.
- [x] T4: `operator.rs` and `state.rs` have one state owner, one recovery
      identity check, one conflict classifier, and truthful snapshot docs.
  - Depends on: none; R-004's operator wording is owned here.
  - Provides: the operator candidate consumed by final validation.
  - Packet: [T4](tasks/T4-operator-state.md)
  - Execution: `ses_e3aeadf3cffeDxvZDSfkmP4b5a`
  - Result: Implemented; `lint-changed PKGS="infra-jobs"`, `test-package
    PKG=infra-jobs` (47/47), doc-tests, and `rustfmt --check` passed.
    `lock_failed_job` keeps the existing `SELECT kind, state, claim_generation
    ... FOR UPDATE` text to avoid new `.sqlx` metadata; its columns are
    deliberately uncompared, and the mutation predicate is authoritative.
- [x] T5: `enqueue.rs` and `trace_context.rs` name the wake admission, state
      carrier validation, and keep one limit owner.
  - Depends on: T1.
  - Provides: the enqueue/trace candidate consumed by final validation.
  - Packet: [T5](tasks/T5-enqueue-trace.md)
  - Execution: `ses_e3ad21373ffelfNpR3jivwLpQh`
  - Result: Implemented; `lint-changed`, `test-package` (51/51), the
    `hotpath` feature check, and `rustfmt --check` passed.

## Completion

After T1 through T5 are Implemented and their writers have joined, assign one
acceptance-unit lead the final delivery boundary: run the assembled checks,
return the Completion result, and hand the validated candidate to the
Orchestrator for commit, push, pull request, and merge. A failed check returns
to its owning unit; only invalidated proof reruns.

## Coverage map

| Recommendation | Task |
| --- | --- |
| R-001, R-002, R-003, R-004, R-010, R-027, R-029, R-030, R-031 | T1 |
| R-009, R-010, R-015, R-018, R-021, R-022, R-028, R-032 | T2 |
| R-007, R-008, R-011, R-012, R-013, R-014, R-017, R-024, R-025 | T3 |
| R-004 (operator wording), R-006, R-019, R-020, R-023, R-033 | T4 |
| R-005, R-016, R-026 | T5 |

## Decisions

- R-029's rename forces caller edits in `crates/infra-messaging/src/outbox.rs`
  and `crates/infra-webhooks/src/{inbound,outbound}.rs`; the T1 packet's
  consumer list is widened to those callers. They return the new `Kinds` and
  register through `KindsBuilder`.
- T1 added one local `#[expect(clippy::struct_field_names, reason = ...)]` on
  `Attempt.attempt_deadline`. The packet mandates the field name; the
  workspace lint policy is unchanged.
- T4 keeps the existing `lock_failed_job` SQL text to avoid new `.sqlx`
  metadata outside its lock; the mutation predicate remains the single
  identity/state check. R-020's state comparison is satisfied by removing the
  Rust literal, not by adding an enum comparison to a query whose selected
  columns are now deliberately uncompared.

## Completion result

Accepted: Completion. The assembled candidate carries all R-001 through R-033.
`make verify` passed 11/11 steps locally (duplication check, unused deps, deny,
dependency drift, fmt, lint, build, test, quality self-test, template
projections, docs-check); 1085 tests passed and 3 were skipped; doc-tests passed
for all 27 crates; `docs-check` found 0 link errors. The claim statement's
`.sqlx` entry was regenerated and `ALLOW_HEAVY=1 make sqlx-check` passed; the
`execution::` real-database suite passed 45/45. The workstation's `sccache`
wrapper leaves the build context unknown, so the local pass publishes no
receipt; the pull-request CI gate owns that receipt. Candidate tree identity
`5ab874a9159d1b750ab1868cc28c2cf18d650a848ccabfe42d092cc1f06634ea` on
`7edc91ab`.
