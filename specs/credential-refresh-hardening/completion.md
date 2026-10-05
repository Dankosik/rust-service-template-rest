# Credential refresh hardening local completion

```text
unit: Completion
verdict: Accepted
candidate: fa371d91c30682dd6f19bb9b392c708129f8786ff7a37c6d94029c7d531c4da3
review: PASS; no surviving findings
next_owner: /root for ledger update, authorized commit/push/separate PR and actual selected CI results
```

Local implementation and its agreed validation are accepted. Matching build,
workspace tests and documentation links passed on the unchanged candidate;
independent assembled review returned PASS. CI, PR publication, release,
deployment and observed rotation remain distinct and are not claimed here.

## Fixed candidate

Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Branch: `codex/credential-refresh-hardening-20261005`.
The SHA256 above covers the sorted list of JSON objects with `path` and
`sha256`, serialized with sorted keys and compact separators. All 32 file
identities were rechecked unchanged after the failed execution attempts.
`completion.md` and `implementation-review.md` are evidence-only exclusions.
All implementation writers and the reviewer are joined. No source, manifest,
lockfile or test repair was made during final validation; root remains the sole
ledger writer.

| Path | SHA256 |
| --- | --- |
| `crates/infra-bearerauthn/src/refresh.rs` | `d476f0bf284e09fc1cc47b90a067d73fbd8c848df34fc54779a8dc429520fc50` |
| `crates/infra-messaging/src/messaging.rs` | `52a1fef9a3746e1e367b51f30538552aec2a971868e71bc5e9e6ea6ffd32ac17` |
| `crates/infra-oauth2-client-credentials/src/lib.rs` | `3f4e92c1d7991d738d6809178038a33e2a9eee9c0a452df11f504981769bda16` |
| `crates/infra-oauth2-client-credentials/src/tests.rs` | `1acedf138042bbf4f1c04cc851853735780f4a373300737851ad8df7b5aaac19` |
| `crates/infra-postgres/src/credentials.rs` | `a82a69cd6a6212452a7e7baf99cfa8c3577ee590bd452822d52d0090384f49b8` |
| `docs/architecture/persistence.md` | `3a86f656472e54e2863dd611eba99208bbf0c11851f754358c44ed0246c0729d` |
| `docs/authentication.md` | `baa7bdf4678fbbf7224c42449bc3f4e70599169e1c9f62e2e487c3a48c0202ba` |
| `docs/background-jobs.md` | `e685838302191a61d27212de943b3f126630d6f4934b6b5f150350e9b478bf89` |
| `docs/cache.md` | `4a748a83d912168927ddae8c280ad5af72b76182372522be3abc461658e881e7` |
| `docs/configuration-source-policy.md` | `2b39e230a887866180d2d62f31c02f29d9b1985c2240f915e22b78b4b4f7696d` |
| `docs/durable-messaging.md` | `84afae9e6c44aed759c922cd04703f9878ff822f7c3eaf7082ec32d6ba9f3047` |
| `docs/grpc.md` | `e79a027915fba0809be29c941703212dcccf8852a031d93ce0642b97c1b00d43` |
| `docs/outbound-http.md` | `b0a73128a9753d679bc517e4fb8776fa7352b6ebf54c6288b331b92c6759773c` |
| `docs/outbound-machine-authentication-decisions.md` | `0f550304b76512fcd2d3f905ae97dfc8aab970f5036f8ee13e2a1c6dc81c5948` |
| `docs/outbound-machine-authentication.md` | `9bf595169f17aa2f93477099c5274e48bbe522797faaccb8c1cfe37134e90858` |
| `specs/credential-refresh-hardening/definition-review.md` | `e948d112947905594b3aa39c956952fd1749de7a35ca0bc3868032e71e6bb733` |
| `specs/credential-refresh-hardening/definition-transition.md` | `2d76d5d0de589889769da18f24e7c0a4fb07bc1c399f1b09ac74fe3dbb74c70f` |
| `specs/credential-refresh-hardening/design-review.md` | `98bd3cd366607f9821413c3205e4ce92520ca3f59288787f78c45317adf7e46c` |
| `specs/credential-refresh-hardening/design-transition.md` | `835c2a20514ced5a5cbc02d7b1de3242107bd6dc424709fdfbc8384be98723b8` |
| `specs/credential-refresh-hardening/design/design-dependency-review.md` | `505e0d1a2a5060370aa0e87bba41698b374424b20c6be93e82b46f15bfd9f013` |
| `specs/credential-refresh-hardening/design/design-dependency-transition.md` | `5722d1a7d1a7b97c3dee0658a51f095f065fb22fe43d5b6d84ea390f3427069a` |
| `specs/credential-refresh-hardening/design/technical-design.md` | `b199de02839fcda51eb162e91e7c96e03236f351e6f91e32e7507c5e89241f11` |
| `specs/credential-refresh-hardening/intent.md` | `9eddcb61174baa0f02b3414c3d2e342e3ef5f072adc4021d5c81ceac79060bf2` |
| `specs/credential-refresh-hardening/planning-review.md` | `54a8682315a42d7da73905cc551eb164d0f9c88a8b514b2bc35a6530bc1e9a0c` |
| `specs/credential-refresh-hardening/planning-transition.md` | `a3ad12011a32d427cb10312d346a67f61eab9015f3f1cf8316aa9e3d130599ff` |
| `specs/credential-refresh-hardening/research/baseline.md` | `07d2f8ccc4acdf264879225957cddad46362ca9198e85d6b18fd5be3a383003f` |
| `specs/credential-refresh-hardening/spec.md` | `afcd3f7d849af2df4d01f89f0ba8547af7cfa8f22ee0165af50e0468fca14401` |
| `specs/credential-refresh-hardening/tasks.md` | `f42f10851f8026126ed14bbb719921a58c6d71a8677070ba8b1da6c1b661feab` |
| `specs/credential-refresh-hardening/tasks/T1-nats-reconnect.md` | `de1630534f68b0cd807576784f7b585f36b8730f56ab22dcae45bc783dd2142d` |
| `specs/credential-refresh-hardening/tasks/T2-oauth-refresh.md` | `7843ab0d4b2ebbda340104cef521ba2592f549882c6e1b2778bd24a09d3b51e0` |
| `specs/credential-refresh-hardening/tasks/T3-jwks-periods.md` | `12fa15ca5e3d5476089ebf359785ec3946914a2ad8d6d9d094de55628af65679` |
| `specs/credential-refresh-hardening/tasks/T4-rotation-guidance.md` | `c5aa3ffc07af0fa040a2aa858893eb6c0c2991aa4dfa69599780acd9524e5c3d` |

## Plan and execution environment

The accepted ordinary local plan is one matching `make build`, workspace
`make test`, `make docs-check`, static source/guide consistency and one fresh
independent assembled Implementation Review. Cargo remains locked. No full
check, local profile matrix, database, broker, identity-provider or rotation
environment was created.

The pinned Rust 1.99.0 aarch64-apple-darwin toolchain was installed. Cargo was
missing only from the shell PATH: the first `make plan` reported
`cargo_unavailable`; prefixing the existing `/Users/daniil/.cargo/bin` corrected
that classification without installation or dependency changes. The corrected
plan passed and classified source/documentation with no manifest/lockfile delta:

```sh
/opt/homebrew/bin/rtk proxy env PATH="/Users/daniil/.cargo/bin:$PATH" make plan
```

It identified the affected Rust owners and dependent crates. The accepted
several-crate criterion uses the one workspace build/test plan. Optional or
CI-owned commands printed by the classifier were not promoted into extra local
acceptance checks. PostgreSQL comments still select DB/sqlx CI surfaces;
initializer runtime, DB/sqlx, messaging and OAuth integration results remain
for the actual PR, alongside its other selected gates.

## Execution evidence

| Claim | Result and scope |
| --- | --- |
| Matching workspace build | PASS: dev profile completed in 6 min 46 s on the fixed candidate. |
| Required workspace tests | PASS on the final continuation: 869 passed, 0 failed, 1 explicitly CI-owned Go-wire fixture case ignored, 0 filtered; 67 successful summaries. Wall 324.63 s, including 2 min 45 s test compilation. |
| Documentation links/fragments | PASS: pinned offline lychee, 1362 total links, 594 unique, 1173 OK, 189 excluded, zero errors; wall 11.51 s. |
| Source/guide consistency | Source-qualified findings below and independent no-findings analysis; no live cutover or revocation proof. |

Exact passing docs command:

```sh
/opt/homebrew/bin/rtk proxy env PATH="/Users/daniil/.cargo/bin:$PATH" /usr/bin/time -p make docs-check
```

It covered all original candidate Markdown. The two subsequent evidence-only
receipts add no Markdown links or fragment references requiring execution.

The first submitted build waited for the existing Git-common lock and exited
75 after its native 900-second timeout, before CPU work:

```sh
/opt/homebrew/bin/rtk proxy env PATH="/Users/daniil/.cargo/bin:$PATH" bash scripts/ci/validation-lock.sh -- /usr/bin/time -p make build
```

Other active validation owners were respected. Root selected one bounded
scheduling recovery that held the lock across both required targets:

```sh
/opt/homebrew/bin/rtk proxy env PATH="/Users/daniil/.cargo/bin:$PATH" bash scripts/ci/validation-lock.sh -- /usr/bin/time -p make build test
```

This acquired the lock, ran `cargo build --workspace --locked` successfully,
then `cargo test --workspace --no-fail-fast --locked`. During test compilation,
rustc reported `IO failure on output stream: No space left on device`; the
jobs-worker lib-test fingerprint could not be written. RTK also reported OS
error 28 and returned exit 1, leaving no complete batch timing summary.
The unchanged sqlx-core emitted a deprecated `Atomic::fetch_update` warning;
no candidate source error was established. The failed batch terminated and
released the shared lock. Build PASS remains valid independently of the
failed test-compilation stage.

At failure, `df -h .` reported 870 MiB free. The task-created target occupied
5.9 GiB: 1.5 GiB incremental, 3.9 GiB deps, 212 MiB build outputs. Root confirmed
bounded private-output recovery within local validation authority. Only the
verified non-symlink `target/debug/incremental` inside this worktree was removed;
no shared cache, other checkout or user data was touched. The next free-space
snapshot was 3.9 GiB. Only tests were retried with incremental output disabled:

```sh
/opt/homebrew/bin/rtk proxy env PATH="/Users/daniil/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 bash scripts/ci/validation-lock.sh -- /usr/bin/time -p make test
```

`CARGO_INCREMENTAL=0` changes cache production, not source, feature selection,
assertions or test semantics. This retry ended with native exit 134 while
queued, before test execution; its log stayed empty. The abort cause is
unestablished and is not labelled as another disk failure. No matching own
waiter or task-worktree process remained at inspection; another task held the
shared lock. The resource snapshot after that abort was 7.6 GiB free, this target 4.8 GiB,
and no incremental directory. No blind retry or further deletion followed.

Task-local raw logs and the machine-readable manifest are under
`/var/folders/9r/ft1t72w13r765bpf61v9mly00000gn/T/credential-refresh-validation-20261005-l3dmfr9e/`:
`build.log`, `build-test.log`, `docs-check.log`, `test-retry.log`, `test-final.log`, `candidate.json`.
These paths are local diagnostics; this receipt retains the durable results.

## Source and guide consistency


This is source inspection, not live authentication or rotation evidence.

| Contract | Inspected owner and matching guide disposition |
| --- | --- |
| NATS scheduling and material | Existing SDK callback consumes the AWS-LC secure-random interface through async-nats/rustls. Immediate attempts, capped spread, fallback, per-challenge coherent JWT/seed reads and reconnect CA limits agree with the messaging guide; no manifest or lockfile change remains. |
| OAuth lifetime | Admission stores one lead, enqueue and successful completion own retry eligibility, and the same cutoff, queue, provider capacity, lock and cancellation owner remain. Access-token refresh is separate from fixed assertion key/kid rotation in both OAuth guides. |
| JWKS worker | One persistent deadline survives unknown-key work; periodic rearm uses current time. Cancellation, pending tickets, unchanged cooldown and last-good publication remain in the same owners, matching authentication guidance. |
| PostgreSQL and LISTEN | Password reads update future pool options; unreadable/empty/non-UTF8 content retains last-good options. Poll cadence is not delivery/authentication completion; max lifetime acts at pool lifecycle points. The separate listener explicitly has no max lifetime and follows options on reconnect. Misleading source comments were corrected without executable changes. |
| Redis | Existing 1-second read plus AUTH, retry of unchanged rejected bytes, conditional 7/11-second local recovery and jittered reconnect are preserved; admitted custom TLS bytes and username stay fixed. |
| TLS and external publication | Fixed gRPC acceptor/config, constructed tonic/reqwest OTLP clients, process-wide outbound HTTP TLS, NATS reconnect CA and SQLx additive handshake roots retain their distinct load points. Atomic single-file publication does not make multiple reads a coherent generation. Existing sessions and resumption are separate from trust updates. |
| Profile closure | Optional cross-provider links in configuration guidance are within matching template markers. Existing integration/lifecycle summaries needed no contradictory-claim repair. Initializer execution remains CI-owned. |

The independent reviewer additionally inspected difft deltas, complete relevant
test owners, native/library interfaces and candidate hashes. No new runtime
control, telemetry axis, credential platform or maximum-key-age policy is added.

## Final test continuation

Root's focused read-only diagnosis found a nearby RTK crash header matching the
queued retry interval; exact attribution remained unproved. No crash body or
secret-bearing process data was read. Root ran one short launcher/lock probe:
`launcher_enter pid=56476`, normal 2-second contention timeout against an active
`make build` owner, and `lock_result=75`; native exit 75. This established only
that the short RTK/Bash/lock path worked again.

Root authorized the same required `make test` with existing compiled outputs,
`CARGO_INCREMENTAL=0` and `CARGO_BUILD_JOBS=1`. The latter changes compilation
concurrency, not features, assertions or test count. A marker-only inline Bash
wrapper captured the outer RTK/launcher, lock waiter and acquired test process
identities; no repository runner or test environment was added. Inherited
lock-bypass variables were unset. The exact launched command is recorded below.

```sh
/opt/homebrew/bin/rtk proxy env -u VALIDATION_LOCK_HELD -u VALIDATION_LOCK_DIR PATH="/Users/daniil/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 bash -c 'printf '\''launcher_enter pid=%s ppid=%s\n'\'' "$$" "$PPID"
bash scripts/ci/validation-lock.sh -- bash -c '\''printf '\''\'\'''\''test_enter pid=%s ppid=%s\n'\''\'\'''\'' "$$" "$PPID"
/usr/bin/time -p make test
task_test_status=$?
printf '\''\'\'''\''test_exit=%s\n'\''\'\'''\'' "$task_test_status"
exit "$task_test_status"'\'' &
task_waiter_pid=$!
printf '\''lock_waiter pid=%s\n'\'' "$task_waiter_pid"
wait "$task_waiter_pid"
task_launcher_status=$?
printf '\''launcher_exit=%s\n'\'' "$task_launcher_status"
exit "$task_launcher_status"' > /var/folders/9r/ft1t72w13r765bpf61v9mly00000gn/T/credential-refresh-validation-20261005-l3dmfr9e/test-final.log 2>&1
```

The continuation acquired the shared lock and completed normally: outer RTK
PID 64330, launcher PID 64331, lock waiter PID 64334, acquired test shell PID
81193. Native session 93103 exited 0; `test_exit=0` and `launcher_exit=0` both
appear in `test-final.log`. `/usr/bin/time` recorded real 324.63 s, user 188.03 s,
sys 32.36 s. Cargo reported the test profile compiled in 2 min 45 s.

All 67 result summaries are OK: 869 passed, 0 failed, 1 ignored, 0 filtered.
The one ignored test is `rust_production_wire_exports_for_go`, whose existing
reason requires CI-generated `GO_WIRE_FIXTURES` from the pinned actual Go
package. No passing live-provider, database, initializer/profile or Go fixture
integration result is inferred from the ordinary workspace suite.

All 32 candidate hashes were rechecked unchanged after success. Earlier failed
resource attempts are superseded for test proof by this completed run; build
and docs evidence remain valid. There were no source repairs or invalidated
source-review claims.

## Independent review and delivery boundary

`implementation-review.md` records the single fresh native Astra/high review
and its evidence-only continuations. Final verdict: PASS, no findings. The sole
prior UPSTREAM_GAP concerned unavailable mandatory test proof and is now closed.
The reviewer independently read the final test log and reused unaffected source
reasoning; it did not rerun checks. All reviewers, writers and this validation
execution are joined.

No further local checks are required by the accepted plan. Root owns the ledger
update, authorized commit/push/separate PR and actual selected CI readback.
Initializer runtime, DB/sqlx, messaging, OAuth and other selected external gates
retain their existing CI authority and are not local passing claims.

Guidance remains source-qualified. No measured fleet distribution, live
provider authentication, credential cutover or hard revocation deadline is
claimed. Merge, deployment, infrastructure and real credential changes remain
outside this outcome.
