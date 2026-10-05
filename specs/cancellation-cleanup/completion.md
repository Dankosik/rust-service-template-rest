# Acceptance Result V1

unit: Completion
verdict: Accepted (local); requested PR/CI outcome pending
candidate: base 78aa3a832bfb4d7e9632ce5ebbbf1680705c31af plus four-file diff
SHA256 a1c0b88245b484fb8d4d0df93df91c0e0c2e1e58f5ef3da76c46e657d3b21426
review: independent Implementation PASS, no findings
next_owner: Lead, authorized commit/push/separate PR and exact-head CI

## Actual local evidence

Environment: macOS, pinned Rust 1.99.0, locked dependency graph. Shell commands
used the required rtk prefix and installed Cargo/OrbStack tool paths. No manifest
or lockfile changes. Only one CPU-heavy check was launched by this Lead at a time.

- `cargo check --locked -p infra-object-storage --tests`: passed (30s static feedback).
- Negative control: remove only `self.body = ByteStream::default();` from the
  assembled implementation, retaining the new regression. `cargo test --locked
  -p infra-object-storage retained_failed_download_releases_body_and_finishes_once
  -- --exact download::tests::retained_failed_download_releases_body_and_finishes_once`
  executed one test and failed the intended destructor assertion: observed 0,
  expected 1 before returning the error. This is the original production
  failure transition, not a compile/setup failure. Restored the fix before
  positive proof. Later extraction of the same test read driver only repaired
  lint nesting and preserved the same read/poll and destructor oracle.
- `make build`: passed, 2m13s, every workspace crate in debug mode.
- `make test-changed PKGS="infra-object-storage service"`: passed, 94 tests,
  zero failed/ignored; 46 object storage, 15 service unit, 15 OpenAPI and 18
  process tests. New regression passed through both read interfaces and all
  four error shapes. Test compilation took 2m23s.
- `make lint-changed PKGS=infra-object-storage`: passed after the test driver
  nesting/length repair; includes the existing integration build variant.
- `make fmt-check` and `git diff --check`: passed.
- `make docs-check`: passed, 1298 checked, 1116 OK, 182 excluded, zero errors.
  Corrected the new object-storage fragment after the first check rejected it.
- `make plan`: selected Rust source, duplication, documentation, object storage
  integration and initializer surfaces. Full initializer and object-storage
  integration are CI-owned. Static review verified both new markers registered
  with the owning optional profiles and the moved jobs marker preserved.

Supplemental `make template-init-projections` first waited on an unrelated
shared lock. Its outer make was interrupted while stopping that optional wait;
the lock had just become available and its child had already captured snapshot
`d49aa064bad0727968a994aae7d0f31b8dee43dc`. The child is being joined separately;
no passing aggregate result is claimed here. It is not an additional local
acceptance gate. Full selected initializer proof remains with PR CI.

## Scope and remaining boundary

Four canonical files: Download owner/test, first-production-feature guide,
object-storage guide, and mechanical profile marker registration. No new
public API, helper, task, crate, configuration, retry or runtime budget.
Plain SQLx and its accepted backport remain untouched. No socket disconnect,
remote cancellation, real-provider, database or deployment proof is claimed.
Merge, deployment and live-provider writes are outside authority.

Accepted rules now live in the canonical guides and Download owner. This
completed execution bundle is archived in Git, then removed from the final PR
diff under Cleanup; this record is not a second permanent decision authority.
