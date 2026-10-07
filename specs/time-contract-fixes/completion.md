# Time-contract corrections — local validation

## Completion Result V1

- unit: Completion.
- verdict: Accepted for the local core correction only.
- candidate: `fb78343a982da9289a691c42c201d9803692c050`, compared with source
  baseline `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. The 14 changed tracked
  files under `crates/` and `docs/` have Git-blob fingerprint
  `cde5b2ccb804f2030ec27ee01fdd21b51cf1758e5f4cbaa8a480434bb10febd0`.
  This receipt is a documentation-only addition after those source checks.
- evidence: matching workspace build and tests pass, with 870 tests passed,
  zero failed, and one existing CI-owned Go wire-compatibility test ignored
  locally. Format and documentation checks pass. The concrete CI duplication
  finding was repaired and its checker passes without policy changes.
- review: PASS from independent reviewer `/root/time_fix_final_review` on the
  exact candidate and canonical fingerprint above. No surviving findings.
  The reviewer verified the T1 fixture cleanup, unchanged production, and the
  actual build/test log and counts without repeating proof.
- invalidated_receipts: the original physical-return deadline premise was
  superseded by reviewed Definition and Technical Design corrections. The
  final operation decision precedes terminal observation, which reports and
  returns that same fixed result. The authentication test-only duplication
  repair and T1 fixture cleanup are covered by the successful workspace suite.
- next_owner: root delivery coordinator for the canonical core ledger closeout,
  receipt commit/documentation-link check, draft PR and remaining CI obligations.
  The webhook business-policy decision remains open outside this core. All
  validation commands/readers have stopped; this writer is released, with no
  source edits, optional proof expansion or further cleanup pending.

## Final Review Result V1

- candidate: `fb78343a982da9289a691c42c201d9803692c050`; canonical 14-file
  Git-blob fingerprint
  `cde5b2ccb804f2030ec27ee01fdd21b51cf1758e5f4cbaa8a480434bb10febd0`.
- reviewer: independent `/root/time_fix_final_review`.
- verdict: PASS.
- findings: none surviving.
- evidence_boundary: the reviewer independently checked the T1 fixture cleanup
  against the accepted operation-decision contract, confirmed production was
  unchanged by that cleanup, and verified successful build/test log SHA256
  `70b02af703f0a6fac30db05e3136d064c5aab033d868b8842169e6d75fc71b96`
  with 870 passed, zero failed, and one existing CI-owned ignored case. The
  reviewer did not duplicate validation. Prior unaffected review evidence and
  the current exact-candidate proof support local core acceptance only.
- reopen_owner: none for the locally accepted core. Root retains separate
  webhook policy, draft PR and applicable CI/release obligations; this verdict
  claims no merge, deployment, live-provider result or whole-request completion.

## Executed evidence

Commands ran in the task worktree with the installed pinned toolchain and
`/Users/daniil/.cargo/bin` prepended only for those commands. All Cargo build,
check and test commands kept `--locked`. Compiler, duplication and build/test
execution used the existing Git-common validation lock; no CPU-heavy checks
were run concurrently within this delivery.

| Command | Actual result | Scope |
| --- | --- | --- |
| `make build test` | PASS; exit 0. Build 6m22s, test compilation 2m35s; 870 passed, zero failed | Workspace deliverables and ordinary workspace suite, using the bounded recovery environment below. |
| `make fmt-check docs-check` | PASS; 1318 total links, 1135 OK, zero errors | Current T1 source/packet cleanup and the recovery-state completion receipt. This final prose refresh does not relabel the earlier run as newly executed. |
| `make duplication-check` | PASS; 147 gated files, 46 report-only test files | Concrete authentication clone finding repaired by shared actor-evidence test cases/assertions; separate engine fixtures and Invalid/Unavailable classifications remain. No admission policy, control or threshold changed. |
| `cargo check --locked -p infra-bearerauthn -p infra-cache -p infra-http -p infra-outbound-http --all-targets --keep-going` | PASS; 1m23s compiler stage after queue | Initial four-owner production/test diagnostics; subsequent test-only changes are covered by the workspace suite. |
| `cargo check --locked -p infra-bearerauthn --all-targets --keep-going` | PASS; 24.24s | Focused compiler refresh after the authentication actor-test refactor. |

The changed owner suites ran without ignored or filtered cases:
`infra-bearerauthn` 68 passed; `infra-cache` 44 passed; `infra-http` 82 passed;
`infra-outbound-http` 26 passed. These include the new retained/fresh
provenance, clock-step, pre-epoch recovery, replacement retention, Redis TTL
admission, Retry-After rounding, fixed outbound end and terminal-observation
regressions. Existing coalescing/error/cancellation and OAuth coverage also ran.

The one ignored workspace test is
`rust_production_wire_exports_for_go`: its existing reason is that CI generates
`GO_WIRE_FIXTURES` using the pinned actual Go package. Feature-gated integration
binaries with zero tests are not counted as integration proof. No live database,
cache, OAuth provider or deployment claim follows from the ordinary suite.
The existing `vendor/sqlx-core` atomic-method deprecation warning was nonfatal
and unrelated to this correction.

## Bounded disk recovery

The first `make build test` attempt failed during build with
`No space left on device (os error 28)`; its tests never started. Root then
removed only this newly created task worktree's generated, non-shared `target/`
after all readers stopped. No other cache or worktree was removed.

The single recovery attempt passed these command-local environment settings:

```text
CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0
CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=2
```

They reduce debug artifacts and compilation concurrency while preserving debug
assertions and optimization. No manifest or global Cargo configuration changed.
The retry began with 6.4 GiB free, showed active dependency compilation and
5.4 GiB free at its first checkpoint, and finished with 811 MiB free. No blind
retry, further cleanup, or optional baseline run followed success.

## Regression-control and identity boundaries

Two optional outbound before-fix controls were attempted by temporarily
replacing only `infra-outbound-http/src/lib.rs` with baseline `5927ffb`, while
keeping the new tests. The first was stopped while queued to repair the CI
finding; the second reached its deliberate 30-second lock-admission limit.
Neither attempt compiled or ran tests. Both restored the candidate
byte-for-byte. There is no before-fix runtime receipt; the successful current
regressions and source evidence have their stated scope.

The candidate fingerprint above hashes sorted changed tracked paths under
`crates/` and `docs/`, each as path bytes, NUL, Git-blob bytes, NUL. Working-file
bytes were compared with those committed blobs after the successful run.
Earlier `f4529946...` was a hash of Git diff text under a different algorithm
and must not be used as this candidate fingerprint.

## Logs and remaining scope

Native logs remain under the repository Git-common directory at
`codex/time-contract-fixes/`: `build-test-recovery.log`,
`fmt-docs-recovery.log`, `compiler-resume.log`, `compiler-auth-repair.log`,
`duplication-repair.log`, the failed `build-test.log`, and the explicitly
unexecuted `outbound-baseline-controls*.log` attempts. The successful build/test
log SHA256 is
`70b02af703f0a6fac30db05e3136d064c5aab033d868b8842169e6d75fc71b96`.

Full-repository, initializer, database/cache/OAuth integration, lint, image and
security gates were not appended for confidence. Existing CI ownership and
requested PR/release obligations remain with root; this local acceptance does
not establish those unrun heavy gates or authorize converting draft delivery
into merge/deployment. The separate webhook retention business decision remains
outside this core correction. Root ran `make docs-check` after the final receipt
and ledger edits: exit 0, 1320 links, 1136 OK, zero errors. This evidence covers
their current relative-link/fragment topology; no new link was added by this
plain-text result entry. Source/test evidence remains unchanged.
