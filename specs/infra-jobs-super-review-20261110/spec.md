# Specification

## Behavior delta

The change is a readability and maintainability refactor. Observable job-engine
behavior stays unchanged. The public Rust paths `Kinds` and `Registry` change as
described in R-029, and the limit items listed in R-030 move to `kind` while
their crate-root re-exports stay available.

## Accepted recommendations

| ID | Outcome |
| --- | --- |
| R-001 | `Job::deadline()` documents the attempt deadline; the private field says `attempt_deadline`. |
| R-002 | `CompleteError::Database` keeps one cause: message plus `source`, not both. |
| R-003 | `From<E> for JobError` documents the pool-timeout snooze exception. |
| R-004 | Policy upper bounds are public and used in docs; the kind-name and cursor diagnostics match their grammar and constants. |
| R-005 | `Carrier` documents its actual stored invariant. |
| R-006 | `JobSnapshot` documents timestamp format and `attempts` meaning. |
| R-007 | `OperationError` documents how transaction begin/commit failures are reported. |
| R-008 | `claim` records the literal-state partial-index invariant. |
| R-009 | `COOPERATIVE_GRACE` records its purpose and supervisor-deadline cap. |
| R-010 | Error-summary construction and the final stored bound each name their reason and owner. |
| R-011 | `statement_turn` is held only while the claim statement uses a pooled connection. |
| R-012 | `Run` owns its task label; a caller cannot pair the wrong label. |
| R-013 | `SupervisorGuard` records its claim-stop and `drained()` consequence. |
| R-014 | `registered_kinds` states its sorted, deduplicated contract. |
| R-015 | `CompletionBatch` derives its id and generation arrays from `queued`. |
| R-016 | The wake admission is named for its mutation and stores one entry per kind. |
| R-017 | Peer registration in `Engine::build` is named and documented. |
| R-018 | The unreachable `Transition::Complete` arm in `execute` is removed; write transitions have a write-only view. |
| R-019 | Operator recovery has one identity/failed check and one named conflict classifier. |
| R-020 | `JobState` owns both stored directions; operator code uses the enum, not raw state strings. |
| R-021 | Metric pre-registration uses the transition and disposition label owners. |
| R-022 | One helper clamps a local wait to the supervisor deadline. |
| R-023 | The operator statement timeout has a named owner. |
| R-024 | The claim statement computes exhaustion and lease rescue once. |
| R-025 | The lease offset has one formula shared by SQL and the local deadline. |
| R-026 | `Carrier::into_stored` is renamed for a same-type validation. |
| R-027 | `Disposition` numeric variants name their units. |
| R-028 | The gauge guard is named for its metric membership, not claim custody. |
| R-029 | The validated kind set is `Kinds`; the builder is `KindsBuilder`. |
| R-030 | Shared delay and error-summary limits live in `kind`, without import cycles. |
| R-031 | `KindsBuilder` has one empty-construction owner. |
| R-032 | `job_failed` uses the documented `job.attempt` key. |
| R-033 | `parse_handled_kinds` states the empty case once. |

## Constraints

- No new dependency, crate, trait, configuration key, or runtime behavior.
- Use the current `main` source (`7edc91ab`); the review snapshot
  (`f77a3534`) differs only in documentation paths and the removal of the
  retention budget. The doc path is `docs/guides/background-jobs.md`.
- Preserve public crate-root paths where a definition moves; update in-repo
  consumers and tests in the same unit.
- SQL changes keep the statement's access paths, `SKIP LOCKED` scans, claim
  ordering, and fenced outcomes. Regenerate `.sqlx` metadata.
- The source checkout's unrelated OAuth2 work and `specs/` bundle stay
  untouched.
