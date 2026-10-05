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

## CI quality repair on published candidate

Status: ready

CI run `37343386370`, quality job `111875855734`, reported four lint
errors on committed candidate `db379f9d14303cba416cbd48b8fe43b86c30852e`:
`unused_async_trait_impl` in the new telemetry test exporter, two
`single_match_else` sites in listener drain, and `assert_is_empty` in its
peer-close assertion. Source evidence is `/tmp/process-lifecycle-ci-quality.log`.
This bounded final-validation repair preserves the preceding local Completion
and review evidence for their immutable scope; it does not relabel the failing
remote quality result as a pass.

The immediate-ready exporter returns `std::future::ready`, the drain uses
`if let`/`is_ok` while keeping the same deadline, join error, force-cancel and
return paths, and the empty-byte assertion displays actual bytes on failure.
No lint allowance or policy changes. Canonical `make lint` and affected
package tests ran serially with the accepted two-job, no-incremental,
zero-debug-info resource profile under the existing validation lock. No cache
cleanup, provider environment, commit or remote write is part of this repair.

The first local canonical lint pass reached the roots and exposed six related
diagnostics hidden behind the original adapter failures: the two stage-unwind
matches, service cleanup function length, worker caught-future size and its
dependent regression future, and the R1 Option mapping shape. These are repaired
with if-let forms, an extracted same-module diagnostics cleanup stage, boxing
of the existing caught worker future, and `map_or_else`. Failure priority,
polling/cancellation ownership, guarded cleanup, configured durations and
public APIs are preserved. No diagnostic was suppressed. The first local
lint log is `/tmp/process-lifecycle-ci-repair-lint.log`; the retry is
`/tmp/process-lifecycle-ci-repair-lint-retry.log`.

Final repair evidence, on the five-file source diff identified below:

| Command after common environment/lock prefix | Result | Scope |
| --- | --- | --- |
| `make lint CARGO=/Users/daniil/.cargo/bin/cargo` | PASS, exit 0; Cargo check 5.26 s | Canonical workspace lint, all targets and selected retained integration features. |
| `make test-changed 'PKGS=infra-http infra-telemetry service jobs-worker' CARGO=/Users/daniil/.cargo/bin/cargo` | PASS, exit 0; 189 passed, zero failed/ignored/filtered | Four affected package suites and their applicable process tests. Test compilation took 1m 51s. |
| Targeted rustfmt; `git diff --check` | PASS | All repaired source formatting and diff whitespace. |

The common execution prefix was `/opt/homebrew/bin/rtk proxy env` with the
PATH recorded in [completion](completion.md), `CARGO_BUILD_JOBS=2`,
`CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, then `bash scripts/ci/validation-lock.sh --`.
All Cargo invocations remained locked. The retry waited for another checkout's
existing validation lock and ran after its release; no overlapping heavy
command or cache cleanup occurred.

Successful logs are `/tmp/process-lifecycle-ci-repair-lint-retry.log` and
`/tmp/process-lifecycle-ci-repair-tests.log`. The only warning is the previously
recorded vendored SQLx deprecation. The 189 cases include HTTP 87, telemetry 27
plus panic-hook 1, worker 16 plus process 8, and service 17 plus lifecycle 15
and OpenAPI 18. Linux-only gRPC process tests executed zero cases on this Mac;
those are not passes. No database/broker/cache/provider or new CI result is
claimed. The prior 875-test receipt remains applicable only to its original
candidate and unchanged surfaces; these focused runs refresh the repaired
surfaces without repeating the unrelated workspace suites.

Source base: `db379f9d14303cba416cbd48b8fe43b86c30852e`. The bounded five-file
`git diff --binary HEAD -- crates/infra-http/src/server.rs
crates/infra-telemetry/src/traces.rs crates/jobs-worker/src/bootstrap.rs
crates/jobs-worker/src/shutdown.rs crates/service/src/bootstrap/shutdown.rs`
SHA256 is `fff54a447a6ef811866ef3a915d21cd415e18d8364aa536315459d06436690ee`. This execution-record update is the sixth changed
path. Public APIs, lifecycle defaults, source-removal markers, dependencies,
locked versions, generated contracts and upstream phase artifacts are unchanged.

CI repair result: `Implemented / HANDOFF_READY`. All descendants remain joined;
this repair used the Lead directly. No source writer, local reader or command
remains active. The continuation root owns bounded review of this delta, its
follow-up commit to the same PR and fresh remote CI results. No commit, push,
PR write, merge or deployment was performed by this repair owner.

## CI duplication admission repair

Status: ready

Quality run `37347419464`, job `111889635427`, passed its lint/build/test
step on `bca8c5073cb70975eadf19479bd3c661f7ff8b39` and reported one unadmitted
production clone. Exact evidence is `/tmp/process-lifecycle-ci-quality-bca8.log`.
The current reported ranges are worker `shutdown.rs:110-127` and service
`bootstrap/shutdown.rs:84-101`, 18 lines / 155 tokens. They contain the native
`Signals` receiver declarations and Unix install entry, not Budget arithmetic.
Both source ranges and their enclosing implementation/call responsibilities
were read back before choosing this repair.

The necessity decision retains these two private owners. TD-1 requires direct
native receiver custody through runtime shutdown; each process keeps its own
first-stop/error/expedite policy. Deleting either owner would lose that
process's receiver lifetime. Collapsing them would need a cross-composition
crate edge or a new shared lifecycle abstraction for the same native fields,
contrary to the closed ownership design without a present shared behavior.
For example, a worker-specific signal-error/expedite change currently belongs
only to the worker; a shared wrapper would couple it to the service or add a
configuration/adapter seam. There is no request for that shared owner.

The P4 production admission uses exactly two path-bound occurrences,
the detected 18-line source text, unique adjacent anchors and 155-token
ceilings. Existing cases are unchanged, as are detector configuration,
thresholds, ignore patterns and every Rust file. The maintenance table in
`docs/build-test-and-development-commands.md` states the retained ownership and
reopen condition. This is a deliberate scoped policy admission, not a baseline
refresh. Fresh independent necessity review and the existing canonical
duplication check completed below. No functional-test rerun is selected for
this policy-only delta.

Independent Review Result V1:

```text
candidate: HEAD bca8c5073cb70975eadf19479bd3c661f7ff8b39 plus frozen baseline/maintenance-document diff SHA256 6dc0d6bdb84bec63ebf211b5ab868574ae779247a3b8b7632ec1dcb8bdbae9e4
reviewer: /root/lifecycle_implementation/signals_admission_review
method: rust-structural-quality necessity/deletion test and reviewed duplication admission policy
native_dispatch: fresh reviewer-agent, gpt-6-astra, high, fork_turns none
verdict: PASS
findings: none
reopen_owner: none
```

The reviewer independently verified both file identities and the fixed diff;
exact source matches at both reported ranges; unique before/after anchors;
exactly two matching source occurrences under `crates/`; 155-token ceilings;
and unchanged earlier admissions, detector controls, checker and Rust source.
Its attempted deletion/extraction falsifier confirmed that removal loses
required receiver custody, while sharing crosses the independent composition
owners without a present shared responsibility. The surrounding error contracts
also differ (worker `SignalError`, service `std::io::Error`). The maintenance
row records that boundary and its reconsideration condition. Review was
read-only and covered this admission's necessity and scope, not unchanged
functional L1-L8 behavior. Native status confirms the reviewer completed.

Canonical `make duplication-check CARGO=/Users/daniil/.cargo/bin/cargo`
completed with exit 0: 147 gated files and 46 dedicated test files scanned;
the dedicated-test result remains report-only. Log:
`/tmp/process-lifecycle-signals-duplication-check.log`.
`make docs-check CARGO=/Users/daniil/.cargo/bin/cargo` completed with exit 0:
1330 links, 576 unique, 1149 OK, zero errors, 181 excluded. Log:
`/tmp/process-lifecycle-signals-admission-docs.log`. `git diff --check` also
passed. Both Make commands used the established task PATH and two-job,
no-incremental, zero-debug-info environment under
`bash scripts/ci/validation-lock.sh --`; they ran serially. Recording these
results adds no new links or policy input after the successful checks.

The reviewed two-file policy diff and file hashes remain unchanged after
review. P4 is the only new case; all preceding case values remain equal to
HEAD. `.jscpd.json`, checker scripts, Rust, dependencies and accepted upstream
phase artifacts are unchanged. The previous functional review and runtime-test
receipts retain their existing immutable scope; no 875-test rerun, environment
creation or extra harness was used for this policy-only repair.

Duplication repair result: `Implemented / HANDOFF_READY`. All writers,
readers and commands have joined. The bounded delta is three files:
`quality/duplication-baseline.json`, the one-row maintenance documentation,
and this execution record. The continuation root owns publication of this
delta to the existing PR and fresh remote CI; this owner performed no commit,
push, PR mutation, merge or deployment.
