# T6 tracing feedback investigation

Implementation against `e20164b3a6430e8ddd416e2f6235105c95d2bf1d`
(base `699887b`), 2026-10-06. The bounded experiment below demonstrated a
missing-span path and informed the test-only repair. Historical CI attribution
and final assembled crate proof remain separate.

## Cause and scope

`Cargo.lock` resolves `tracing` 0.1.44 and `tracing-core` 0.1.36.
`the_span_names_the_region_only_on_amazon` already created a second live
dispatcher in `c17770a964000a9c4d0f92927d4ac06804412305`; later `4683533`
changes other object-storage tests. The supplied historical zero-versus-one
failure and unchanged pass did not establish a landed repair.

The exact pinned `tracing-core` permits this ordering:

1. `DefaultCallsite::register` publishes its callsite and obtains a rebuilder
   (`callsite.rs:308-320`).
2. With at most one registered dispatcher, `Rebuilder::JustOne` reads the
   registering thread's default without taking the registry read lock
   (`callsite.rs:544-548,568-577`). An unrelated thread may have no subscriber.
3. New dispatchers rebuild the callsite against the interested capture
   subscribers (`callsite.rs:484-487,551-559`).
4. The earlier registration can subsequently store its older `never` result
   (`callsite.rs:490-508,357-365`), overwriting that rebuild. `span!` tests
   cached interest before consulting the current subscriber (`tracing`
   `macros.rs:55-75`); no `new_span` or `record` callback follows.

The experiment gated a sole subscriber's first `register_callsite` before it
returned `never`, installed the capture and second dispatcher, then released
that earlier registration. This controls the exact overwrite ordering instead
of relying on sleeps or stress repetitions. It reproduced an enabled capture
receiving zero Amazon spans. The gate models the delayed uninterested result;
it does not prove this was the actual historical CI interleaving.

`OperationGuard::start` supplies the Amazon `cloud.region` attribute at span
creation before the presign SDK await. The test has a current-thread Tokio
runtime. No production instrumentation defect was found. `Records`' constant
span ID and flat record lines do not explain the missing creation callback.
A Registry/Layer substitution uses the same callsite registration and would
not by itself close the observed ordering.

## Bounded causal observation

The root admitted one tracing-only scenario after the T7 writer joined and
released the shared validation resource. The command acquired that lock
immediately at 2026-10-06T16:43:24Z:

```sh
/opt/homebrew/bin/rtk proxy env VALIDATION_LOCK_TIMEOUT_SECONDS=5 bash scripts/ci/validation-lock.sh -- /opt/homebrew/bin/rtk proxy python3 .artifacts/t6-tracing-probe/run.py
```

The runner used installed `rustc 1.99.0 (b940084d7 2026-09-28)` directly, with
no debug info or incremental cache. Dependencies were exact local source:
`once_cell` 1.21.4 (`std,alloc,race`), `pin-project-lite` 0.2.17,
`tracing-core` 0.1.36 (`std,once_cell`), and `tracing` 0.1.44 (`std`).
Each local `.crate` archive SHA-256 matched its `Cargo.lock` checksum, and
installed source files matched the archive bytes. No Cargo resolution,
manifest/lock edit, dependency download, container or other target was used.
An initial invocation stopped before compilation because this Cargo registry
has no `.cargo-checksum.json`; the archive comparison replaced that assumption.

The recorded callbacks were:

```text
deny.register_callsite object_storage -> held before never
capture.register_callsite object_storage -> always
capture.register_callsite object_storage -> always
deny.register_callsite object_storage -> released never
operation.enabled=false
region.new_span_count=0
assertion one Amazon operation span: left 0, right 1
EXIT 101 (expected)
```

The same capture setup, operation callsite and assertions in a fresh process
produced `operation.enabled=true`, one `new_span` carrying
`cloud.region=eu-central-1`, a second provider span with no region, and two
`record` callbacks carrying the successful outcomes; exit 0. Both actual
phases and exact compiler invocations remain in the local diagnostic log.

Compile and both phases took 3.435 seconds; retained files were 3,758,060 bytes.
The admitted limits were 120 seconds, 64 MiB retained and at least 250 MiB
free disk, checked during execution. All owned processes joined; the shared
lock was released. The earlier 2 GiB fixture floor was explicitly clarified
by the root as inapplicable to this tiny admitted diagnostic.

## Repair and retained proof owner

Only the two capture tests now invoke their existing test executable with an
exact test filter, matching the repository's telemetry isolation pattern.
The parent owns a 20-second bound and kills/reaps a stalled child. It also
requires the child's entry marker, so an exact filter that matches zero tests
cannot pass silently. Each capture
therefore starts with fresh callsite/dispatcher state, without unrelated
libtest threads entering the operation callsite during dispatcher setup.
Other object-storage tests retain normal parallel execution. This introduces
no production seam, dependency, global subscriber or blanket serialization.

All prior assertions remain. The region test additionally requires one
`S3.PresignGetObject` span after Amazon and two after R2, so a missing R2 span
cannot satisfy the negative region assertion. Presign still uses the real
operation; its existing no-network assertion remains owned by
`presign_is_bounded_and_redacted`. The request-identifier test preserves its
real HTTP stub and existing identifier/redaction assertions.

Test authoring rationale: the existing tests own the telemetry contract;
the demonstrated late registration can make the interested capture miss it.
Extending those owners avoids duplicate production tests. The diagnostic is
causal feedback for the selected repair, not final crate validation. No full
object-storage build or test suite ran in this lane. Completion still owns
matching build, actual changed telemetry tests/crate proof and final review.
The historical CI root cause remains unattributed beyond the demonstrated
compatible failure path.

## Local diagnostic identities

The ignored `.artifacts/t6-tracing-probe/` directory retains the source,
runner, callback log and small executable; these are task-local evidence,
not a new committed validation runner. SHA-256:

- `probe.rs`: `746e34fd0462f0766b1859a52c2e436878a5a4293f0e1491b2846cc3c600d261`
- `run.py`: `1c707c6971e523309ee0baf7ac0795111fa3fa1a4dc85d371a097f853e96b2d8`
- `commands-and-results.log`: `913e4b28513d3878b0239bc16fedf326e04fe89f468bb60e40ff82d7a52a6f3d`
