# Process lifecycle implementation

Status: ready

The single unit in [plan](plan.md) is implemented at base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, branch
`codex/process-lifecycle-20261005`. Accepted upstream phase artifacts are
preserved. Code production is not final acceptance.

## Execution custody

The native Acceptance-Unit Lead owns service composition, manifests, joined
integration, and this record. Disjoint implementation lanes produced the
HTTP/telemetry adapters, PostgreSQL/messaging admission adapters, worker
composition and docs. Each lane received the ready design and the same-unit
boundary; none ran aggregate checks, product tests, review or publication.

Two necessary caller/compatibility surfaces extend the plan's writable list:
`crates/jobs-worker/src/operator.rs` mechanically consumes fallible native
signal receivers without changing one-shot policy; `scripts/lib/template_profiles.json`
tracks added/removed source-removal markers. The existing native failure guard
in `crates/infra-jobs/src/engine.rs` also reports panics after cancellation,
which otherwise disappear because its tracker owns completion rather than a
returned JoinHandle. The worker consumes that native failure after joining.
The continuation owner confirmed this remains L3/L7 repair; engine admission,
claiming and durable settlement APIs do not change.

## Implementation and feedback

Retained resource slots, independent task/listener observations, async unwind
boundaries, staged cleanup and whole-tail accounting are implemented in both
roots and their adapters. `futures-util` remains resolved at 0.3.34; service
uses its normal unconditional edge and worker retains that edge without jobs.
Cargo.lock already contains the service edge from its former dev declaration,
so no lock bytes need hand editing or version resolution changes.

Serial compile-only feedback completed. Initial HTTP connection wrapper
capture produced E0277; moving its inner future captured the owned values
without adding a `Sync` API bound. The final command completed with exit 0:

```sh
/opt/homebrew/bin/rtk proxy env PATH=/Users/daniil/.cargo/bin:/Users/daniil/.nvm/versions/node/v25.8.2/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin cargo check --locked -p service -p jobs-worker -p infra-postgres -p infra-messaging -p infra-http -p infra-telemetry -p infra-jobs -p integration-tests --all-targets --features integration-tests/integration,infra-messaging/integration --keep-going
```

This covers affected production, ordinary tests and the changed existing
feature-gated fixture tests without executing them. The remaining warning is
the unchanged vendored sqlx-core `Atomic::fetch_update` deprecation at
`vendor/sqlx-core/src/pool/inner.rs:229`; new unused imports were removed.
Targeted formatting, `git diff --check`, and a changed-file marker inventory
readback completed with no issues. No aggregate build/test/review or runtime
provider proof ran; compile feedback is not an acceptance receipt.

Owner-adjacent coverage now includes full listener expiry/Drop/accept-error
cleanup with live peers, explicit telemetry attempts/deadline clamping,
retained pool and broker admission cancellation through existing fixtures,
background panic after cancellation, forced acknowledgement, and 43.5-second
equality. The service case runs real startup through a second-bind refusal, then proves
the retained first listener serves a live peer and closes it during common
cleanup without readiness propagation. Worker coverage exercises registered
work cancellation/join after registration unwinds. Behavioral
execution, including regression evidence, remains unrun at this handoff stage.
No provider service or test environment has been created.

## Next owner

All descendants have returned and native state confirms they are completed:
`admission_adapters`, `listener_telemetry`, `worker_lifecycle`, and
`lifecycle_docs`. The R1 repair below is complete; descendants remain joined and no command or writer is active. The
continuation root owns the one assembled final-validation and independent
Implementation Review, then publication. Same-unit repairs return to this
Implementation Lead. No commit, push, PR, merge or deployment occurred.

Ordinary local commands selected by the plan are `make build`, `make test`,
and `make docs-check`, with the task-scoped Cargo path. Existing changed-path
routing owns feature/profile, real-database, broker and image/CI categories;
this record adds no new local environment or validation matrix.

## Initial implementation result (superseded by R1 repair)

```text
unit: fixed process lifecycle repair L1-L8
verdict: Implemented
candidate: tracked diff SHA256 bdbb6432b6579346e0792be8cbfb03e0d1d024b1cc77b55defb564862901fbdd; plus specs/process-lifecycle/implementation.md
provides: retained lifecycle ownership, fault observation, guarded unwind, bounded truthful cleanup, aligned docs and owner tests; behavioral proof unverified
next_owner: continuation root for one assembled final-validation and independent Implementation Review, then authorized single-PR delivery
```

The tracked diff contains 32 paths. Accepted plan, spec, design and evidence
SHA256 values still match Planning's identities. Existing phase artifacts are
unchanged and remain part of the root's eventual publication candidate.

## R1 repair

The [Implementation Review](implementation-review.md) found one task defect:
after the running worker consumed its first native stop notification,
`pending_end` could consume a queued second notification before shutdown's
expedite wait. The repair changes only that transition in
`crates/jobs-worker/src/bootstrap.rs`: it inspects `pending_failure`, preserving
sticky task/listener/engine/consumer failure precedence, then classifies the
already received signal. It does not poll or consume another signal. Native
signal ownership, the first observed stop timestamp and all stage budgets
remain unchanged.

The regression `first_stop_preserves_a_queued_second_stop_for_drain` runs the
production transition with both native SIGTERM and SIGINT queued. Independent
receivers acknowledge both broadcasts before the transition is polled. The
next native wait must remain immediately ready, and `first_stop` must remain
the same. The test invokes only itself in the existing unit-test executable
to keep process-wide signals away from sibling lifecycle tests. This adds no
production injection seam, dependency, provider service or separate runner.
It is Unix-gated because it exercises those native signal types.

The worker-only compile diagnostic completed with exit 0 under the existing
shared validation lock:

```sh
/opt/homebrew/bin/rtk proxy env PATH=/Users/daniil/.cargo/bin:/Users/daniil/.nvm/versions/node/v25.8.2/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin bash scripts/ci/validation-lock.sh -- cargo check --locked -p jobs-worker --all-targets --keep-going
```

Only the previously recorded vendored SQLx deprecation warning remains.
Targeted rustfmt and `git diff --check` also completed. The regression was
compiled, not executed; no pre-fix runtime failure or post-fix runtime pass is
claimed. Its executable proof remains with the delivery owner. No aggregate
validation or review ran during repair.

This repair changes only the worker bootstrap owner and this execution record
relative to the initially reviewed candidate. The four original descendants
remain completed; this repair was made directly by the Lead with no new lane
or overlapping writer. Accepted plan/spec/design/evidence hashes still match.
The initial FAIL review remains authoritative until its owner rechecks R1.

## Corrected Acceptance Result V1

```text
unit: fixed process lifecycle repair L1-L8, anchored R1 correction
verdict: Implemented
candidate: tracked git diff --binary HEAD SHA256 feeee793c49468d2f384127484f89bb27e32e668ea1061283f0fce90a8f6e8c3; plus specs/process-lifecycle/implementation.md
provides: R1 preserves the second stop notification and original failure/deadline semantics; regression compiled and awaiting execution
next_owner: continuation root to delivery actor and retained reviewer for bounded R1 recheck, then assembled build/test/docs and authorized single-PR delivery
```

`HANDOFF_READY`: all writers joined, no command active, no commit or external
write performed. The previous tracked diff identity is superseded by this
corrected candidate; unaffected review reasoning remains with the same reviewer.
