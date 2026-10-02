# OAuth closeout local completion

Date: 2026-10-02.

unit: Completion

verdict: Accepted for local delivery under the [plan](plan.md).

candidate: branch `codex/oauth2-client-credentials-closeout-20261002`, HEAD
`7f0c85e67eff8c02194bc3dbb77efdf57548b3ef`, plus ten changed source/operator-documentation
files. Their `git diff --binary HEAD` SHA256 is
`b19549655940ad7fc0d37a1205d9962b2b2a8ee7c9b1bc28aa69874820e58ac0`.
The completion and review files are evidence added after that frozen candidate;
they contain no runtime changes.

## Delivered behavior

The adapter now shares completed service acquisition failures for one second,
keeps omitted lifetimes request-only and rejects unrepresentable lifetimes,
and uses Moka weighted retention for the configured entry target and 16 MiB
Bearer payload target. `Credentials::prepare` returns credentials and an owned
`RefreshDriver`; the integration drives and awaits it. Terminal driver closure
rejects new HTTP and gRPC work before cache reuse or dispatch. The existing
five-second background budget includes queue/lock waiting. Explicit normal
Tokio `sync` and `macros` features support the driver without gRPC.

All in-repository fixture constructors and four operator/lifecycle documents
use the owned lifecycle. The source migration is intentional:
`Credentials::new` is removed. Derived consumers must retain, drive and await
the returned driver under their existing shutdown/join owner. No concrete
provider is added to bootstrap, and no unmanaged fallback is retained.

## Local evidence

All commands ran in the named worktree with pinned Rust 1.99.0, `--locked`
Cargo commands, and shared cache
`/Users/daniil/Projects/Opensource/rust-service-template-rest/target`.
PATH included Cargo, Homebrew and `/usr/local/bin` for Docker. CPU-heavy commands
were serialized through `scripts/ci/validation-lock.sh` in the Git-common
directory; unrelated worktree checks were allowed to finish.

The manifest and documentation surfaces were confirmed by `make plan`.
One tailored plan covered workspace compilation/tests, affected feature routes,
documentation, and CONTRIBUTING's manifest dependency/secret checks. No full
matrix, heavy integration environment, or repeated workspace run was added.

| Command | Result and exercised scope |
| --- | --- |
| `git diff --check`; `make fmt-check` | Pass for the final source candidate. |
| `make build` | Pass, matching workspace build; compile finished in 6.88s. |
| `make test` | Initial execution completed all targets with `--no-fail-fast`: 814 passed, 2 OAuth fixture failures, 1 explicitly ignored CI-owned NATS wire case. Its 759 non-OAuth passing tests and process tests remain valid after the test-only OAuth repairs. |
| `make test-package PKG=infra-oauth2-client-credentials` | Fresh repair rerun: 57 passed, 0 failed/ignored, including gRPC; 11.13s test execution. This closes both initial failures and rechecks the shared fixture/lifecycle correction. |
| `cargo check --locked -p infra-oauth2-client-credentials --no-default-features --lib` | Pass, normal library without gRPC; 0.29s wall. |
| `cargo test --locked -p infra-oauth2-client-credentials --no-default-features --lib` | Pass: 44 tests, 0 failed/ignored; 20.99s wall including compilation, 10.25s test execution. |
| `make docs-check` | Pass over operator and task documentation; initial run 904 links, zero errors; final receipt-only link refresh also passed. |
| `make deny` | Pass: advisories, bans, licenses and sources; allowed duplicate-version warnings remain in the unchanged locked graph. 1.98s wall. |
| `make secret-scan BASE_REF=origin/main` | Pass: no leaks in worktree or two-commit range; base `546a381aa74286bce5336b5b59c7bf46bf6f3bae`. 4.88s wall. |

The first frozen diff was
`7fdd7ae620a928c97873591cb2491282baa7e288539e3e0196182a01ccd75826`.
Final validation exposed two fixture defects before the intended behavior was
proved. Repair changed only OAuth tests: the fixture admits valid large Bearer
headers, the expiry scenario isolates the prior owner's suppression and waits
for the correct request, and the driver scenario observes replacement dispatch
before a large clock advance. No production/source-interface change invalidated
the matching build or other workspace results. The failed initial OAuth result
is superseded only by the fresh package rerun; no synthetic all-green aggregate
receipt is claimed. The workspace's ordinary local scope is covered by the
759 reusable non-OAuth successes plus 57 fresh OAuth successes.

Stable raw command logs, final source patch and batch timing/result JSON remain
under the Git-common directory at
`.git/codex/oauth-closeout-final-20261002/`. Named logs are `build.log`,
`test.log`, `oauth-repair-test.log`, `no-default-check.log`,
`no-default-test.log`, `docs-check.log`, `docs-check-final.log`,
`fmt-check-final.log`, `deny.log`, `secret-scan.log`; the batch receipt is
`remaining-results.json`. The source patch is `accepted-source.patch`.
These logs establish local observations, not external CI or deployment.

review: [Fresh final Implementation Review](implementation-review.md) PASS,
same reviewer retained for bounded test repair; no remaining findings.

## Remaining external owner and stop

next_owner: root continuation owner commits/pushes this accepted delivery,
updates the existing separate PR #219, marks it ready when appropriate, and
obtains the selected final-candidate CI results. Draft-only green status cannot
establish the skipped Keycloak or initializer/profile gates. Local lint,
unused-dependency, integration and profile matrices were not duplicated for
confidence; existing selected CI gates remain intact.

Keycloak, profile initialization/removal and other selected CI outcomes are
pending their real run. No live-provider, memory/RSS, performance, merge or
deployment result is claimed. The local Lead stops at accepted delivery and
remains available for in-scope CI repair. Merge, deployment, provider/platform
migration and DPoP rollout remain outside this outcome.
