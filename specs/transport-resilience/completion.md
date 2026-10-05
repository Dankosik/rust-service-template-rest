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
- Last published/CI revision: `7c483dd0ca9db4a759c7966c374f416a77f99154`.
- Current delta: six source/provenance files below, plus root-owned `tasks.md`
  and this evidence-only report. No runtime implementation, manifest or lock
  changed in this latest repair.
- Authority: [specification](spec.md), [design](design/transport.md) and
  [execution boundary](tasks/execution-boundary.md).

| Current repaired file | SHA256 |
| --- | --- |
| `crates/infra-messaging/src/messaging.rs` | `748b0d13f4bf8088bddb911c54d3d7deb2017621a9d8d607e6ac1b38add423f9` |
| `make/template.mk` | `8c4a8ee07728604eedd5dc77113667a209dabc047b42a43324db3ddf31e242dd` |
| `test/tests/postgres.rs` | `f2089e0038330a9df8718ece781fd77e3b1fdffd23482558665d5c355df33e39` |
| `vendor/sqlx-core/PATCHES.md` | `3ff3fc8ab16215b408f04fb8ab4483920c278290e7a22d7dee7df6a9115e3fb7` |
| `vendor/aws-smithy-http-client/src/client.rs` | `6a56f0515c6d69933cf7aef6f81550b50b3c0d26e0b40b6bfeca50cfc1ec78fd` |
| `vendor/aws-smithy-http-client/PATCHES.md` | `f3dd9a8adb3e7684d451d90b75010529a4f49ac29412bfc398af4ed8cacac69e` |

The final Smithy two-file manifest is
`/tmp/transport-smithy-fixture-repair-candidate.json`, file digest
`50dd8d5b418ef3b941cce53814ad50a30b8dd1ea3e9ed4a74e1599ad215d1c4c`.

## Actual CI evidence

[Run 37344471286](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37344471286)
observed the published revision above and finished **failure**. Job status and
relevant logs were reopened before this report was updated. These are scoped
results from that revision, not a successful run of the current unpublished
repair.

| Observed scope | Result |
| --- | --- |
| Workspace build within `make --keep-going lint build test` | PASS; debug build finished in 40.84 s. |
| Workspace tests | 871 passed, 0 failed across 67 suite summaries. One intentionally ignored `rust_production_wire_exports_for_go` case belongs to the pinned actual-Go integration scenario. |
| Changed auth/gRPC regressions | Both auth stalled-DNS/TLS recovery cases and gRPC `stalled_tls_dial_is_released_and_the_same_client_recovers` passed in that workspace run. |
| `docs`, `delivery`, `security` | PASS. Delivery covered changed shell, tool manifest and Dockerfile checks; security covered dependency review/policy. |
| `initializer (projections)` | PASS, 368 canonical selections. This is source projection proof, not the runtime initializer matrix. |
| `quality` overall | FAIL: test-only lint diagnostics and the excluded SQLx package's old root test selector. The bounded repairs are implemented; later quality steps were skipped and are not passes. |
| `secrets` | FAIL on this report's original command/commit notation. All eleven published-fixture findings were cleared. The notation was repaired without another exception; current worktree scanning then passed. |
| Integration, OAuth integration, runtime initializer matrix and image | Intentionally deferred by the draft route; no runtime/image result established. |
| `required` | FAIL, faithfully reflecting failed jobs. |

The production build/tests remain useful for unchanged runtime source and the
production dependency graph. Current test-only, command-routing, provenance and
report changes still need their actual next CI results. The real PostgreSQL /
PgBouncer, provider compatibility and image claims retain their existing owners.

## Executed native proof

The three existing native harness filters ran serially using their published
locks and the task-private target. No new runner, environment or matrix was
created. The final positive results are:

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
matches the current table. The first repair receipt predates a documentation-only
PATCHES update; the final two-file manifest above owns the current provenance
file identity.

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

Root can publish the bounded reviewed repair and obtain the next exact-candidate
selected CI results. It owns the guarded replacement of its latest repair
commit, preserving the initial implementation commit and the established
publication bounds. Existing deferred integration, runtime initializer, image
and applicable analysis gates remain required for their later scope. No merge
or deployment follows. Completion remains pending actual required CI and the
final evidence update; no additional local full build, scanner or matrix run
is scheduled by this report.
