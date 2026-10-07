# Transport resilience — Completion Result V1

Date: 2026-10-05. Status: **implementation complete; final CI verification pending**.
All six units are Implemented. Published PR [246](https://github.com/Dankosik/rust-service-template-rest/pull/246)
has current scoped proof and a bounded repair awaiting publication. Root owns
[ledger](tasks.md), commits, publication and final CI readback. No merge,
deployment or production observation is claimed.

## Candidate and authority

- Checkout: `/Users/daniil/.codex/worktrees/transport-resilience/rust-service-template-rest`.
- Branch: `codex/transport-resilience-20261005`.
- Accepted source base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
- Last published/CI revision: `7a337196d0cd44b998a4bb19ff1816402523a8cc`.
- Current source delta: one `Box::pin` around the existing bounded PostgreSQL
  test future; root-owned `tasks.md` and this report are evidence updates.
  Runtime source, deadline, join/cancellation ownership and assertions are unchanged.
- Authority: [specification](spec.md), [design](design/transport.md) and
  [execution boundary](tasks/execution-boundary.md).
- Repaired `test/tests/postgres.rs` SHA256:
  `97916f8ac7ef1cd361227bf0173ed86d1d809968a6f01440358d9f0d711aece4`.

## Actual CI evidence

[Run 37350119107](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37350119107)
observes the published revision above. Its quality log was reopened and counted;
root's same-revision job readback supplies the other statuses below. This is a
progress snapshot, not successful CI for the unpublished heap-placement repair.

| Observed scope | Result |
| --- | --- |
| Workspace build within `make --keep-going lint build test` | PASS; debug build finished in 38.84 s. |
| Workspace/process/doc tests | 871 passed, 0 failed. One intentionally ignored `rust_production_wire_exports_for_go` case belongs to the actual-Go integration scenario below. |
| Native filters in the same quality job | SQLx 3, NATS 9, Smithy 1 passed. Across all 70 suite summaries: 884 passed, 0 failed, 1 intentionally ignored. Native graphs retain the boundaries below. |
| `quality` overall | FAIL solely on Clippy `large_futures` at PostgreSQL test line 117. The one-hunk `Box::pin` repair is implemented; its lint result awaits next CI. Later quality steps were skipped and are not passes. |
| `secrets`, `security`, `delivery`, `docs` | PASS. Prior fixture-policy and report-notation findings are closed at this published revision. |
| `image` | PASS under the existing image job; this does not claim deployment. |
| `integration` | PASS: PostgreSQL, cache, object storage, JetStream and actual Go/Rust wire steps all succeeded. |
| OAuth Keycloak integration | PASS. |
| `initializer (projections)` | PASS, 368 canonical selections. |
| Initializer source part | PASS. |
| Seven runtime initializer parts and Rust CodeQL | Still running at root's readback; root owns terminal results. |

The original auth/gRPC regressions and prior native negative-control evidence
remain valid; the current test-only heap placement changes no runtime behavior.
The final candidate still needs actual lint and selected CI completion. No
successful aggregate is inferred from the 884 passed tests, and no local
build, scanner, projection or matrix rerun is added.

## Executed native proof

The three existing native harness filters ran serially using their published
locks and the task-private target. No new runner, environment or matrix was
created. These local positives are now accompanied by successful runs of all three filters
in the current CI quality job:

| Harness | Result | Scope |
| --- | --- | --- |
| SQLx | 3 passed, 0 failed/ignored, 2 filtered; 0.13 s | Tokio 1.52.1, `_rt-tokio`; native TCP candidate progress, empty resolution and resolver-order failure. |
| NATS | 9 passed, 0 failed/ignored, 82 filtered; 0.31 s | Tokio 1.52.3/rustls 0.23.40; native attempt, fallback, recovery, ownership and close cases. |
| Smithy | 1 passed, 0 failed/ignored, 26 filtered; final rebuilt run 1.31 s | hyper-util 0.1.20/runtime-api 1.16.2; healthy-second TCP fallback and inner-I/O classification. |

The production workspace instead retains Tokio 1.53.1/rustls 0.23.45 and
hyper-util 0.1.21/runtime-api 1.18.0. Native harness results retain their stated
graph boundaries; they do not replace production, TLS-provider or database
integration proof.

All native commands used `/Users/daniil/.cargo/bin/cargo`,
`CARGO_TARGET_DIR=/Users/daniil/.codex/worktrees/transport-resilience/rust-service-template-rest/target`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_INCREMENTAL=0`, and `-j 2`; NATS and Smithy
also used `CARGO_PROFILE_TEST_DEBUG=0`. Executed arguments:

```text
cargo test --locked --manifest-path vendor/sqlx-core/Cargo.toml --no-default-features --features _rt-tokio --lib net::socket::tests -j 2
cargo test --locked --offline --manifest-path vendor/async-nats/Cargo.toml --no-default-features --features jetstream,aws-lc-rs,nkeys --lib transport_resilience -j 2
cargo test --locked --offline --manifest-path vendor/aws-smithy-http-client/Cargo.toml --no-default-features --features rustls-aws-lc --lib same_family_candidate_fallback_and_inner_timeout_classification -j 2
```

SQLx/NATS execution is retained in `/tmp/transport-native-execution.json`.
Smithy repair and negative-control receipts are
`/tmp/transport-smithy-fixture-repair-result.json` and
`/tmp/transport-smithy-negative-control.json`. Their runtime source identity
matches the reviewed Smithy manifest
`/tmp/transport-smithy-fixture-repair-candidate.json` (file digest
`50dd8d5b418ef3b941cce53814ad50a30b8dd1ea3e9ed4a74e1599ad215d1c4c`).
The first repair receipt predates a documentation-only PATCHES update; that final
manifest owns the reviewed provenance identity. The present PostgreSQL-only
repair does not alter these inputs.

Smithy's original refusal fixture left TCP pending on macOS, producing an outer
timeout. The repaired fixture releases a fresh loopback port and verifies actual
`ConnectionRefused` before using it. It passed 1/1. Removing only the production
inner TCP timeout setter then failed at the intended healthy-second fallback
with the outer one-second timeout. Exact source restoration and a fresh
recompile passed 1/1 again. One intervening attempt reused the negative binary
because restored source mtime preceded that build; the owner corrected the
mtime and verified an actual rebuild. The final receipt, not that stale-artifact
attempt, supplies positive proof. No runtime patch, manifest or lock was changed.
No observed pre-fix result is claimed for the SQLx/NATS regressions.

## Policy and diagnostic closure

Both new vendor archives and all nine flagged fixture files were checked against
published checksums/bytes. Nine exceptions require exact path AND exact detected
value for only the named rule; default rules and history-baseline policy remain.
Their NATS/S3 marker groups are registered for removal with the respective
vendors. The pinned [Gitleaks 8.30.1 contract](https://github.com/gitleaks/gitleaks/blob/v8.30.1/README.md#configuration)
and full-PEM match support those exact-content restrictions.

The existing policy scan passed worktree (12.91 MB) and the original one-commit
range (2.62 MB). Controls yielded 0 findings for unchanged published fixtures;
changed values at allowed paths, relocated originals and additional values each
yielded 11 findings (6 private-key, 4 JWT, 1 generic-api-key). After the report
notation repair, worktree scanning passed again over 14.99 MB. No payload values
were emitted in diagnostic output.

For this command record only, `$review_base` denotes the literal revision passed
when the successful range scan ran; it does not describe a different invocation.

Comparison revision: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

```text
make secret-scan BASE_REF="$review_base" GITLEAKS=/opt/homebrew/bin/gitleaks
```

Earlier assembly fixes closed NATS iterator collection, native `wait_closed`
borrow lifetime, scoped config/test lints, classifier fixtures, vendor README /
shell defects, exact initializer vendor-directory admission and migration-path
resolution. The SQLx test recipe now selects its real standalone dev harness.
Initial ENOSPC and Docker loss interrupted local aggregates; no shared cache or
foreign worktree was cleared. Valid native and CI results above supersede the
old blanket unavailable-build/test/docs narrative. The local verification
self-test's ENOSPC interruption still supplies no aggregate receipt.

## Independent review and remaining action

The original assembled review and its bounded fixes retain no surviving source
finding. Fresh security-policy/profile review also found none; the retained
reviewer closed the four-file locator/test-lint/SQLx-selector delta. These
reviews preserve their recorded scope and do not turn failed CI into a pass.
The retained reviewer also closed the final Smithy fixture/provenance change:
no surviving finding, exact negative/restored hashes verified, and final rebuilt
1/1 PASS accepted at its recorded native scope. All readers are joined. Review
disposition remains **NEEDS_PARENT** solely for publication and final selected
CI; no source repair remains from the reviewed deltas.

The retained reviewer closed the latest PostgreSQL test heap-placement hunk
without findings and verified its exact hash. The ten-second deadline, joined
future ownership/drop cancellation, errors and assertions remain unchanged;
actual repaired-candidate Clippy proof stays pending. Readers are joined and
the disposition is NEEDS_PARENT for that proof and final selected CI. Earlier
source/security/native reviews remain current. Root is collecting the running initializer/analysis results
before publishing another bounded repair, so independent failures can be handled
together. It owns publication and terminal exact-candidate CI readback. No merge
or deployment follows. Completion remains pending actual required CI and the
final evidence update; no additional local execution is scheduled by this report.
