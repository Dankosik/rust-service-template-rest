# T2 — Bound complete S3 GET custody by its original deadline

Outcome:
The existing operation timeout bounds GET from its first execution through
confirmed EOF/integrity. Every consumer shares one final decision, and an open
unpolled Download releases all adapter-owned active resources on expiry.
Source/operator guidance describes this complete-GET contract consistently.

Consumes:
- T1 Implemented output and released whole-file
  `docs/configuration-source-policy.md` owner — implementation scheduling gate;
  no T1 test/review/acceptance result is consumed.
- [Spec S1](../spec.md#s1-finite-complete-s3-get-lifetime),
  [D1](../spec.md#d1-precise-unchanged-capacity-and-workload-guidance) and
  [compatibility](../spec.md#compatibility-proof-and-next-boundary) — accepted
  deadline, finality, resource scope and compatibility tightening.
- [Pre-header flow](../design/mechanism.md#s1-absolute-deadline-and-pre-header-flow),
  [shared state](../design/mechanism.md#s1-shared-state-terminal-transitions-and-cleanup),
  [timer custody](../design/mechanism.md#timer-ownership-termination-and-runtime-boundary)
  and [documentation/projections](../design/mechanism.md#documentation-projections-and-collision-boundary)
  — fixed original-end checks, shared synchronous terminal transition, Weak
  timer/exit guard/owned JoinHandle, cancellation and native cooperation.
- [Ownership](../design/ownership.md#responsibilities), its
  [inverse map](../design/ownership.md#files-inverse-map-for-all-expected-rust-changes)
  and [guide map](../design/ownership.md#non-rust-files-and-projection-custody).
- [Dispositions R04/R05/R15](../recommendation-dispositions.md) — S1 owns GET
  custody; #248 owns allocation/provider-response ceilings and #244 owns upload
  yielding. Their immutable heads are collision evidence, not dependencies.
- Existing `crates/infra-object-storage/src/{lib,download,tests}.rs` at source
  `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, current SDK/Tokio features and
  gRPC `call.rs` custody pattern — existing owners, no new dependency required.

Provides:
Implemented S1 with executor-authored proof and storage-specific D1 source,
guide and lifecycle updates. Together with T1, this is the assembled candidate
that unlocks the one final validation and delivery-review boundary.

Boundary:
Capture the original end before GET work; prevent expired first dispatch and
late metadata/body/success decisions. Transfer that end and all active
resources into the existing Download owner, including the empty-object EOF
path. Keep provider body, withheld chunk, permit and observation under one
shared Open state, with terminal extraction and cleanup outside its mutex.
Every consumer uses the same polling/finality owner. Preserve stable failed
reads, validated final-chunk/EOF success, supported checksum/length semantics,
size hints and metadata under the accepted design. No read future owns a held
last chunk across cancellation.

Own one Weak timer with its exit guard captured before first poll and its
JoinHandle in Download. Preserve fail-closed unexpected timer exit, synchronous
resource release on drop, actual task-termination evidence, panic cleanup and
bounded cooperative polling. Keep the distinction between requesting abort and
proving timer completion. No process task registry, producer queue, bootstrap
stage or shutdown budget is added.

Replace existing headers-only/indefinite-custody comments and guide statements;
explain the unchanged timeout key/default/range, full GET lifetime, presign or
adequate existing timeout for slow readers, caller-owned retained bytes and
operation-owned timer cleanup. Preserve PUT/HEAD/DELETE/presign/readiness,
retry/provider mappings and configuration values. Do not change bytes()
allocation policy or import #248's response interceptor; only adapt custody
access required by S1. Existing body.rs/observe.rs/error.rs, manifests/lockfile,
generated APIs and bootstrap remain read-only parity authorities.

Mutable owners:
- `infra-object-storage` GET/Download lifecycle and existing tests:
  `crates/infra-object-storage/src/lib.rs`, `download.rs`, `tests.rs`.
  Private owner-local test sections are permitted under the accepted inverse map.
- Storage configuration field comments only:
  `crates/config/src/object_storage.rs`.
- `docs/object-storage.md`, `docs/object-storage-decisions.md`.
- Existing storage sections of `docs/configuration-source-policy.md`,
  `docs/architecture/runtime-lifecycle.md`, `docs/architecture/integration.md`,
  preserving their current profile markers and T1's gRPC changes.
- This packet's executor-selected proof/command notes; implementation status,
  receipts and execution identity remain root-owned in `../tasks.md`.

Exclusive locks:
- `docs/configuration-source-policy.md` whole-file edit ownership after T1's
  Implemented handoff. No T1 writer remains active when this unit starts.

Final validation:
- Claim: The original full-GET deadline governs pre-header work, every returned
  consumer and empty EOF; no late dispatch/payload/success occurs while Open.
  Expiry releases unpolled body/chunk/permit/observation custody, terminal
  decisions remain final, timer work finishes/cancels, and all observations and
  slots finalize exactly once under the accepted races and cancellation rules.
- Checks: Matching repository build/relevant tests and actual changed
  documentation/profile validation, consolidated with T1 after all code is
  assembled and writers join. Executor chooses discriminating tests and
  commands while implementing; use existing available fixtures/dependencies,
  without provisioning a test environment. One independent final assembled
  review covers changed custody and interaction with G1/D1.
- Observable: An expired unpolled GET cannot retain active adapter resources
  or prevent fresh admitted work; resumed consumers see its stable failure,
  while success chosen before expiry remains successful. Existing integrity,
  cancellation and optional-profile behavior remain consistent with updated
  source/operator guidance. No RSS ceiling or live-provider result is claimed.

Reopen if:
Concrete evidence falsifies original-end custody, cleanup, finality, native
cooperation or the accepted existing-owner placement: Technical Design.
New behavior/config/dependency/feature, changed resource scope or incompatible
provider contract: Definition. Refresh only affected #248/source collisions;
never import unmerged work as a prerequisite. Routine code/test repairs and
command selection remain executor-owned.


## Executor-selected proof and feedback notes

Implementation uses the accepted original deadline and existing Download owner.
It includes the reviewed failed-body framing correction: a failed Download has
unknown upper size hint and false end-of-stream, so Hyper must observe its error.
Open/successful bodies keep their exact length and metadata remains unchanged.
`bytes()` retains its original allocation policy. No manifest/lockfile, provider
response interceptor, bootstrap task or new configuration surface was added.

Proof selection (authored, not yet executed):

- Primary public regression:
  `tests::an_unpolled_get_expires_and_admits_fresh_work`. Existing loopback Stub
  delays headers 400 ms inside a one-second GET budget. An occupied-slot wait
  must finish by 1.2 seconds from the original get start without polling the
  Download; then fresh public HEAD succeeds, repeated direct/body reads fail,
  and the one unavailable operation observation survives owner drop. Returning
  that expired Download through the existing HTTP/1.1 Stub with `Body::new` and
  no explicit Content-Length must fail transport/body consumption, never produce
  a clean empty response. The fixture confirms the request reached the server.
  This jointly distinguishes indefinite custody, restarted header/body budget,
  duplicate observation and the independently established Hyper framing gap.
- Pre-header polling boundary: expired preparation never polls dispatch, and a
  provider poll returning after the end cannot commit its result.
- Private Download owner proof: cancelled read preserves the withheld last chunk;
  successful EOF and integrity failure remain final after the original end;
  timer expiry destroys both provider body and tracked withheld bytes; empty EOF
  and each consumer share the deadline; timer abort before its first poll fails
  closed; drop and caught provider panic release custody synchronously; late
  payload/EOF cannot escape; ready empty frames allow another Tokio task to run.
  Owned handles are joined or native task completion is observed under a bound.
- These cases use current crate dependencies and existing fixtures. This crate's
  own dev Tokio features do not enable paused time, so time-dependent cases use
  short original deadlines plus bounded joins/resource notifications; explicit
  sleep-until is used only to cross an already-finalized operation's old deadline.
  No production test hook, public export, environment or runner was added.
- Existing checksum, exact-length, empty GET, provider mappings and response-body
  tests remain the parity owners. New private cases cover resource/timer and
  cancellation behavior that the public network fixture cannot directly observe.

For the required pre-fix comparison at assembled Completion, transplant only
`an_unpolled_get_expires_and_admits_fresh_work` into source baseline
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af` using its unchanged imports and Stub.
The expected behavioral failure is the occupied-slot timeout with the unpolled
Download retained. Do not transplant the new helper-specific test or the new
private Download tests into baseline, which would only prove a compile failure.
A second focused negative control can restore Failed exact-zero hint on the
implemented candidate and require the HTTP consumer assertion to fail.

Bounded feedback actually run:

- First bare rustfmt/Cargo invocations found those binaries missing from this
  shell PATH; installed pinned 1.99.0 binaries were then used explicitly.
- Targeted formatting:
  `/opt/homebrew/bin/rtk proxy /Users/daniil/.cargo/bin/rustfmt --edition 2024`
  on the three changed storage source files and configuration comment file.
- Compile-only command (shared validation lock; default worktree target):
  `/opt/homebrew/bin/rtk proxy env PATH=/Users/daniil/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin bash scripts/ci/validation-lock.sh -- cargo check --locked -p infra-object-storage --tests`.
  Earlier iterations passed in 37.89 s and 1.37 s; the final corrected source
  passed in 1.24 s after an unrelated validation-lock owner released it. Logs are
  `/tmp/overload-t2-cargo-check.log` and
  `/tmp/overload-t2-cargo-check-final.log`; the latest log is reserved for final
  source feedback (exit 0, including the consumer/hint and lifetime test edits).
- Scoped `git diff --check` passed on the final code/docs and packet notes.
  Bounded diff identity is returned to the root.

No runtime tests, behavioral pre-fix comparison, aggregate validation, final
review, CI, commit, push or external effect ran in this unit. They remain owned
by assembled Completion; compile feedback is not behavioral acceptance.
