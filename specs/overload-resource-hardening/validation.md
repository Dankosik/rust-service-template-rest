# Overload hardening — Validation evidence

This receipt supports the one [Completion result](completion.md); it is not a
synthetic `make verify` receipt. Source stayed fixed during each consuming run,
and readers joined before repairs or baseline substitutions.

## Candidate and route

Base `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`; final tracked diff SHA256
`5db8c3d7f9c2d900d42aa205b9803acf70cefda8d79d6794e6c68643dbf2bf44`.
The final Rust fingerprint is
`92ad16f93726722ea2fa2b85792cf4a574245701a9d2f8c303ff559c8e144547`:
take the sorted `git diff --name-only` paths ending in `.rs`, concatenate each
UTF-8 path, NUL, exact file bytes, NUL, then SHA256 the result. The review
independently verified both identities; baseline substitutions restored exact
saved bytes before final validation and review.

`make plan` with Cargo on PATH selected lint owners `infra-grpc`,
`infra-object-storage`, `service-config`, and an eight-package test closure.
The actual/user-supplied AGENTS table requires `make build` and `make test`
for several changed crates, so one workspace test run covered that closure;
`test-changed` was not run again. No manifest, lockfile, schema, CI or agent
instruction surface changed. Canonical plan log:
`/tmp/overload-completion-plan.log`.

All commands used `/opt/homebrew/bin/rtk proxy` and the pinned Rust 1.99.0.
Cargo discovery prepended `/Users/daniil/.cargo/bin` to the existing PATH.
CPU work serialized through `bash scripts/ci/validation-lock.sh --` using the
Git-common lock. Runtime build, tests and baseline comparisons all used the
existing worktree target with `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`; debug assertions and
optimization semantics were unchanged. Cargo commands remained `--locked`.
No cache deletion, new environment or test runner was introduced.

## Executed checks

| Check | Result and scope | Native evidence |
| --- | --- | --- |
| `make build test` | Exit 0. Build 1m45s; test compilation 1m31s. 875 passed, 0 failed, 1 ignored across 67 test/doc target summaries. gRPC transport 32/32; storage 55/55, including every new case. | `/tmp/overload-completion-build-test.log` |
| `make fmt-check` | Exit 0 on final source. | `/tmp/overload-completion-fmt-final.log` |
| `make lint-changed PKGS='infra-grpc infra-object-storage service-config'` | Exit 0 after mechanical repairs. Reused for unchanged production/tests; final fixture URL expression is the explicitly uncovered delta described below. | `/tmp/overload-completion-mechanical-final.log` |
| `make duplication-check` | Exit 0; 147 gated files, 46 report-only test files. | Same mechanical log |
| `make unused-deps` | Exit 0; existing redundant `rcgen` ignore warning in bearer-auth manifest, no unused dependency defect. | Same mechanical log |
| `make quality-check-self-test` | 12/12 passed in 9.171s. | `/tmp/overload-completion-quality.log` |
| `make docs-check` | Product/accepted packet check passed: 1,432 links, zero errors. Completion receipts receive the final separate docs check recorded below. | `/tmp/overload-completion-docs.log` |
| Scoped `git diff --check` | Passed; independently repeated by reviewer. | Native command and review receipt |

Final receipt documentation check also passed: `make docs-check`, exit 0,
1,443 links and zero errors, including the three Completion/review/evidence
documents. Log: `/tmp/overload-completion-docs-final.log`. Recording this result
adds no link or fragment target beyond that checked set.

The single ignored ordinary-workspace test is
`rust_production_wire_exports_for_go`: its existing declaration states that CI
generates `GO_WIRE_FIXTURES` with the pinned actual Go package. It is not changed
proof silently skipped by this delivery. Database/emulator/live-provider
integration was not claimed from ordinary workspace results.

## Behavioral regression comparison

Commands below are `cargo test --locked` with the listed package, target and
exact selector, followed by `-- --exact --nocapture`. Every comparison used the
shared lock and exact source backup/restoration; no reviewer consumed a mutated
baseline. Command/result records:
`/tmp/overload-completion-regressions.json`.

| Fixed wrong behavior | Selector | Observed result |
| --- | --- | --- |
| Baseline storage lib/download/tests plus only the public regression transplanted | `-p infra-object-storage --lib tests::an_unpolled_get_expires_and_admits_fresh_work` | Compiled and failed at `unpolled GET must release its slot at the original deadline: Elapsed(())`. GET returned after delayed headers, proving the fixture reached body custody. `/tmp/overload-regression-s3-baseline.log` |
| Baseline gRPC router with candidate transport tests | `-p infra-grpc --test transport opening_admission_counts_auth_followers_and_recovers_after_cancellation` | Compiled and failed with `Unauthenticated` instead of `ResourceExhausted`. `/tmp/overload-regression-grpc-opening-baseline.log` |
| Same baseline router | `-p infra-grpc --test transport rejected_ready_empty_frames_cooperate_with_other_tasks` | Compiled and failed because ready empty drainage gave no peer-task progress. `/tmp/overload-regression-grpc-cooperation-baseline.log` |
| Candidate with only Failed hint restored to exact zero, and direct hint assertion adjusted to let the actual HTTP consumer decide | Same public storage selector | Compiled and failed at `expired GET must not become a clean empty HTTP response`. `/tmp/overload-regression-s3-zero-hint-repaired-fixture.log` |

All four restored-candidate behaviors passed in the final workspace suite.
Private storage cases additionally exercised held-chunk destruction, read
cancellation, empty EOF, all consumers, finality, timer abort before first poll,
native task completion, synchronous drop/panic cleanup, late payload/EOF and
cooperative ready empty frames. Existing integrity/provider mapping tests remain
the parity owners; no live S3 result is inferred.

## Repairs and evidence reuse

Initial lint caught T1's oversized fixture function and T2's large GET future,
enum-size/style diagnostics. The T1 owner extracted private connection response
handling. T2 boxed the SDK send future at its owner, collapsed the timer poll
condition, and documented a site-local enum-size allowance because State already
lives in one Arc allocation. The subsequent three-owner lint passed.

The first HTTP framing negative control found a fixture error: requesting `/`
never reached Stub's wildcard route. The sole later test delta changes
`reqwest::get(&consumer.endpoint)` to
`reqwest::get(format!("{}/download", consumer.endpoint))`. This is a fixture URL
expression change, not a claim of identical AST. The repaired framing negative
control and final full suite exercised it. Earlier S3 baseline slot-retention
evidence remains valid because its failure occurs before this consumer branch.

A redundant post-URL scoped lint request never acquired the shared lock, held by
an unrelated broad validation at candidate
`aba175690c0dd726f6e3b3f0be7ae789eb67e898`. Only this delivery's waiting process
was terminated and joined (exit 143); the unrelated owner was not stopped.
Root applied Evidence Contract reuse to unchanged surfaces, retaining the exact
URL delta and latest CI lint obligation. No exact-final lint pass is claimed.
Final formatting independently passed without a heavy-lock requirement.

## Native projection limitation and CI obligations

`make template-quality-projections` materialized immutable candidate
`45ce5a9864987b791c29d740a3a704064c17f5b4`, then stalled in the unchanged
`template_state.py::_batch_blobs` path: Python writes the full object-ID list
before reading Git stdout. The parent and `git cat-file --batch` child were both
sleeping at zero CPU with no projection progress. After a bounded checkpoint,
only that owned Git child was terminated; the native wrapper unwound with
`Broken pipe` and exit 2 after 131 seconds. This is not a projection pass.

Receipt:
`/Users/daniil/Projects/Opensource/rust-service-template-rest/.git/codex/template-init/attempt.Ix4GP8`.
Its failed entry records log SHA256
`fd16fdc6b92c24ce7e05052f0a14114135b75214aeb1b73dc50b3a11e0a7af5a`.
No unrelated validator repair or new runner was introduced. Static profile
inspection passed in the independent review; execution remains for Linux CI.

The canonical route also names `make template-init-check` and
`make test-integration-object-storage` as CI-owned. Root retains these,
source-quality projections, latest lint and all applicable exact-head PR gates.
No `ALLOW_FULL`, `ALLOW_HEAVY`, local CI impersonation or live-provider run was
used. These pending external observations do not expand the ordinary local
build/test/review acceptance boundary.
