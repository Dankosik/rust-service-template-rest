# PostgreSQL pool resilience — Completion

```text
unit: Completion
verdict: Accepted
candidate: source tree e114d7998992419a67e258144a6ef9b4c7fc8b96 against base 67be869acea112af271ec8ba621cbc50ae9d36b7
review: implementation-review.md — fresh independent PASS, no findings
next_owner: LEDGER_ORCHESTRATOR for canonical ledger closure
```

This is local acceptance of the assembled [T1–T3 outcome](tasks.md), under the
accepted [specification](spec.md), [design](design/design.md) and
[source custody](design/dependency-custody.md). The two Completion/review records
were added after proof; they do not change the reviewed source tree. There were
no source repairs during final validation. No push, main-checkout integration,
CI run, image build, release or deployment was performed.

## Candidate and source identity

The worktree is
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-postgres-pool-resilience-20261004`.
All validation used rustc `1.98.1 (48a229cea 2026-09-01)`, with
`/Users/daniil/.cargo/bin` prepended to PATH and existing Docker access retained.
Cargo resolution/execution stayed locked. CPU-heavy commands ran serially through
`scripts/ci/validation-lock.sh`; there was no active writer or source reviewer
during a check's source substitution.

The published sqlx-core 0.9.0 archive SHA256 is
`05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb`.
Of its 110 files, only `src/pool/connection.rs` differs; `PATCHES.md` is the sole
additional file. Patched source SHA256:
`ad99491e0ca834b125da83c65506f70e9f4188d13107368cd66d17b53cd2339c`.
Pristine negative-control source SHA256:
`94269a532ef60aaa31321e43af17cba2424f919a36501d862328ed05958a08de`.

Final `cargo metadata --locked --offline --format-version 1` resolved 588
packages. Comparison with the baseline normalized only sqlx-core's source
identity and the separately authorized `integration-tests -> tracing` dev edge;
all package versions and resolved features remained identical. The lock parser
confirmed exactly the core source/checksum removal and that test edge. The core
resolves to the actual excluded vendor manifest, not a workspace member.

## Executed local plan

Commands below ran from that worktree. The shared-lock prefix and PATH setup
above applied; they are not repeated in every row.

| Command or existing owner | Result and scope |
| --- | --- |
| `make --silent build` | Passed in 50.05 s; workspace build. |
| `CARGO_INCREMENTAL=0 make --silent test` | 804 passed, zero failed; 67 result groups. One existing actual-Go-wire fixture test remains ignored for its CI-provided fixture and is not counted as executed. |
| `make --silent changed-surfaces-check` | Passed; includes vendor-only source, manifest and provenance routing. |
| `make --silent shellcheck SHELL_FILES=scripts/ci/changed-surfaces.sh` | Passed with the pinned ShellCheck 0.11.0 container. |
| `make --silent docs-check` | Passed on the assembled source: 954 total links, 425 unique, zero errors. |
| `make --silent deny` | Advisories, bans, licenses and sources passed. Existing duplicate-version warnings remain; the resolved versions did not change. |
| `make --silent dockerfile-check` | BuildKit check complete, no warnings. This does not establish image build/runtime behavior. |
| Existing `template-profile-projections.py::_project` / canonical initializer, followed by `cargo metadata --locked --offline --format-version 1` | PostgreSQL retained with gRPC absent: 490 packages, real patched core retained/excluded from workspace. PostgreSQL absent with gRPC retained: 475 packages, core/vendor and Cargo/Docker references absent, gRPC exclusion retained. No build/profile/database matrix was multiplied. |
| `CARGO_INCREMENTAL=0 bash scripts/ci/test-integration-db.sh --test postgres -- --nocapture` | 34 passed, zero failed/ignored, in 38.49 s; actual PostgreSQL 18.6 and PgBouncer 1.26.0 from the pinned existing Compose fixture. |

The PostgreSQL target covers silent cancellation at pooled SQL, transaction SQL,
pre-commit and COMMIT boundaries; readiness cancellation; repeated bounded return
after successful SQL; pending-BEGIN disposal and cancelled acquisition; healthy
reuse, finality, existing session/pooler contracts, named acquisition diagnostics,
and responsive saturation/recovery under current readiness policy. The observed
saturation case withdrew readiness after `11.016038375s`; useful work and readiness
recovered `1.333125ms` after release with pool maximum one. These are local
observations, not a production latency or fleet-stability promise.

Two SQLx test-database deletion warnings occurred for PgBouncer-held sessions;
the outer owned Compose teardown removed the fixture. Readback found no task
Compose containers. This does not claim immediate termination of arbitrary
production backends.

## Regression negative control

The existing Compose owner supplied the same PostgreSQL/PgBouncer setup. Only
`vendor/sqlx-core/src/pool/connection.rs` was temporarily replaced by its verified
published bytes. No manifest/lock change or shared registry-cache edit occurred.
The restoration guard retained the exact patched bytes.

Both runs used the existing test runner and the same command:

```bash
CARGO_INCREMENTAL=0 bash scripts/ci/test-integration-db.sh --test postgres \
  a_successful_statement_still_has_a_bounded_silent_return -- --exact --nocapture
```

An owned `CARGO_TARGET_DIR=/tmp/pool-resilience-validation-build.qdYVJU` kept the
remaining compilation outside the worktree's externally deleted target path.
The verified published source compiled and failed with exit 101 at the intended
assertion: `the silent connection releases its local pool slot without a reply`.
One test failed after 7.65 s, with 33 unrelated tests filtered out. This rejects
the original retention defect, not a setup or compilation error.

After exact source restoration, the same test recompiled against the patch and
passed in 11.18 s, exercising two successive silent returns. The working bytes
matched the staged tree again, and `git write-tree` remained
`e114d7998992419a67e258144a6ef9b4c7fc8b96`. The owned Compose fixture was removed.

## Interrupted attempts and reuse

The first workspace test compilation exhausted disk space after the matching
build had passed. Only this worktree's real `target/debug/incremental` directory
was reclaimed (2.9 GiB); the failed test step then passed with incremental
compilation disabled. Shared caches and unrelated paths were not removed.

After the 34-test PostgreSQL success, an external process removed the worktree's
entire target directory while the first negative-control build was running.
Cargo reported missing fingerprint files before a test assertion. That attempt
is not behavioral evidence. The source was restored, the fixture was cleaned,
and only the negative/restored-positive pair was retried in the owned temporary
build directory. No global janitor was stopped or unrelated chat contacted.
The passing build, unit and full PostgreSQL receipts remain valid for their
unchanged covered bytes and were not rerun.

## Retained command receipts

These host-local logs/receipts preserve the exact run output or explicitly named
owner transcription. Their hashes identify the evidence consumed by review;
they are not substitutes for current CI or deployment receipts.

| Evidence | SHA256 |
| --- | --- |
| `/tmp/pool-resilience-build-unit.log` | `426d4cf8191b1b692b1b3edec321f0cf1b8359251666172c4910c87d32d2c2c1` |
| `/tmp/pool-resilience-unit-retry.log` | `b984f907d8f25aa18424af7e8f106cc7aa7d8b276dd0876943cb62912dc802e0` |
| `/tmp/pool-resilience-postgres-positive.log` | `f40d0a1949529e675e16b174a2fc00329e16d8dfbfb5587df8fb42f3d9af94e9` |
| `/tmp/pool-resilience-postgres-negative-final.log` | `b28bd12eca26e80fe3ffc751b2ca890d183a602e1003199a56a92ead7dba6471` |
| `/tmp/pool-resilience-postgres-restored.log` | `9d09f3db762fb454841ff01e64f78f5ec0f2b8d42bfcc8caf304161834676018` |
| `/tmp/pool-resilience-dependency-delivery.log` | `965a8692c210201b29adcd353bb1be790b3f67e53e8d3fe7f21a4d9f66a80134` |
| `/tmp/pool-resilience-projection-results.json` | `1ae79e2cacac27dcffe9e5d92a6deb34c56660e04127e9d6357d2f1478f97e19` |
| `/tmp/pool-resilience-final-metadata.json` | `a619c036d4c93182c67b1da6c90393d6d17a8082613a8e4a00e28bea615090c1` |
| `/tmp/pool-resilience-mechanical-receipt.json` | `d82d7e59734cbbb3734c053ec5c2c73829559243ff3e9dc0ce2124a395a01154` |

## Remaining external scope

Runtime-image build/lifecycle/scan, the full initializer matrix, CodeQL,
Dependency Review and other selected CI gates remain unexecuted and pending CI.
The Dockerfile check and two source projections establish only their stated
local scope. Current PostgreSQL capacities, three-second acquisition, readiness,
PostgreSQL 14+ compatibility contracts, finality and healthy reuse remain the
accepted policy. No production-optimal sizing, release or deployment is claimed.

## Closeout records

The new Completion/review Markdown records received a scoped `docs-check`:
five links, zero errors. The authored-source whitespace check passed. The
preserved historical probe log and exact unified-diff context in `PATCHES.md`
retain their original trailing/context whitespace; they were not rewritten to
satisfy a cosmetic diagnostic. No behavior or source integrity was affected.

All checker and reviewer work finished before handoff. The completed task-owned
temporary Cargo build directory was removed; command logs/receipts were retained.
No shared cache, unrelated checkout or global janitor was changed.
