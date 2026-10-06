# Credential rotation observability delivery

status: done

Completion: The assembled change exposes the bounded file-refresh observations
and successful JWKS acquisition time defined by [R1/R2](spec.md), with accurate
operator documentation, and supplies executed real-server authentication proof
for the NATS and Valkey outcomes in R3 through the existing local/CI route.
One delivery owner establishes the matching local build/tests and applicable
changed-surface checks, resolves final independent delivery review, and records
the exact candidate and actual proof scope. Publish the authorized separate PR
against main, preserving the selected CI gates and obtaining their actual
results; missing required R3 execution remains outstanding. Local acceptance,
PR/CI evidence and requested proof completion are reported distinctly. No
merge or deployment is included.

Global constraints: [Intent](intent.md), [ready Specification](spec.md),
[ready Design](design/design.md), [Implementation](../../docs/spec-first-workflow/phases/implementation.md),
[Validation budget](../../AGENTS.md#validation-budget) and
[Build Speed](../../docs/build-speed.md) govern. Root becomes the sole
`LEDGER_ORCHESTRATOR`; fresh Leads implement units and return `Implemented`.
The T4 Lead is also the single delivery owner after all four units are assembled
and every writer has joined. Tests are authored with code; there is no preceding
test plan, per-task validation, review or acceptance gate. Eligible bounded coding
diagnostics follow Implementation. All heavy Cargo/build/test execution across
checkouts serializes through the existing Git-common validation lock; no shared
cache cleanup, shared target directory, machine/global configuration change or
new environment/runner is authorized. Use only synthetic fixture material and
the already accepted harness changes. Exclusions in Intent remain in force.

## Tasks

- [x] T1: Operators can distinguish completed file-refresh outcomes at each existing provider boundary.
  - Depends on: none.
  - Provides: canonical R1 counters and provider documentation; existing-owner regression coverage.
  - Packet: [T1](tasks/T1-file-refresh-observations.md).
- [x] T2: Operators can observe the latest successfully admitted usable JWKS acquisition time.
  - Depends on: none.
  - Provides: canonical R2 gauge and acquisition semantics in the existing authentication guide; owner regression coverage.
  - Packet: [T2](tasks/T2-jwks-acquisition.md).
- [x] T3: The existing Valkey suite proves authenticated file replacement and recovery at the real server boundary.
  - Depends on: T1 implemented R1 cache telemetry and released cache guide ownership; no passing proof receipt required.
  - Provides: isolated authenticated ACL regression coverage and its existing-suite documentation.
  - Packet: [T3](tasks/T3-valkey-authentication.md).
- [x] T4: The existing messaging integration route proves NATS file replacement, old/expired rejection and recovery on an authenticated broker.
  - Depends on: T1 implemented R1 challenge telemetry and released messaging guide ownership; no passing proof receipt required.
  - Provides: authenticated target, synthetic trust assets, sequential same-broker lifecycle/CI routing, removal ownership and guide updates.
  - Packet: [T4](tasks/T4-nats-authentication.md).

## Results

Execution owner: `/root`, bound as `LEDGER_ORCHESTRATOR` after consuming the
ready Planning transition. Root is the sole canonical ledger writer. Native
collaboration controls provide fresh unit Leads and independent review.

All four units are Implemented and assembled; their writers are joined and
scopes released. The single final delivery boundary is now held by the resumed
delivery owner named below; the original T4 Lead supplied its implementation.

| Unit | Native Lead | State | Dispatch receipt |
| --- | --- | --- | --- |
| T1 | `/root/credential_followup_counters` | Implemented; writers joined | `default`, `gpt-6-astra`, `high`, `fork_turns=none`; native spawn accepted |
| T2 | `/root/credential_followup_jwks` | Implemented; writers joined | `default`, `gpt-6-astra`, `high`, `fork_turns=none`; native spawn accepted |
| T3 | `/root/credential_followup_valkey` | Implemented; writers joined | `default`, `gpt-6-astra`, `high`, `fork_turns=none`; native spawn accepted |
| T4 | `/root/credential_followup_nats_delivery` | Implemented; writers joined; final delivery assigned | `default`, `gpt-6-astra`, `high`, `fork_turns=none`; native spawn accepted |

The callable native inspector exposes identity/status only; no richer effective
model readback is claimed. Final validation, delivery review and PR/CI evidence
are pending; implementation state does not claim behavior passed.

T2 provides the owner-held gauge, matching owner tests and authentication guide.
Root checked its shared-checkout file identities: `refresh.rs`
`4d2cb89198865a03f067479fc1ec1cd8cc7729acd52fe6d6df921f3bfbc6abdd`,
`docs/authentication.md`
`c53a1c3f899340f17ea583cca33c038d2d5d0a9904fa14725acd23dd5ae42556`.
Eligible all-targets compile-only feedback passed for infra-bearerauthn under
the shared validation lock; runtime tests remain unexecuted. Task-local Cargo
feedback used `CARGO_PROFILE_DEV_DEBUG=line-tables-only` and
`CARGO_PROFILE_TEST_DEBUG=line-tables-only`; use the same process-local profile
settings for this private target to avoid duplicate artifacts. No global settings
were changed. This is implementation/coding-feedback evidence, not acceptance.

T1 provides canonical counters, owner regression coverage and provider guides;
bounded seven-file diff SHA256
`0a84e061d25b430c147046266e95f79a243eae5652c8075bea405ccc772817ed`.
Root checked all seven shared-checkout file hashes against the Lead result.
Only formatting ran; behavior and aggregate checks remain unverified until the
single assembled delivery boundary. T3/T4 consume this code without a proof gate.

T3 provides the real-Valkey fixture/relay and guide. Root checked the Lead's
three file hashes. Eligible integration-target compile feedback passed after a
borrow repair; real-server execution is not yet claimed.

T4 provides the registered authenticated NATS target, synthetic trust assets,
same-service runner/CI custody, profile removal and guides. Bounded content
manifest SHA256 `0ad05f456bfca0116d71b921768eb2f11a70b6d0e629839ce4f4289e53bf682d`;
root checked the target, runner and authentication configuration hashes against
the Lead result. Integration-target compile feedback passed; runtime/gates/
review/publication remain unverified. All four Leads reported no active writers
or running diagnostics.

Final delivery starts only now. Local build/link capacity is constrained: disk
fell to roughly 529 MiB and a ledger update hit ENOSPC. The existing ledger
remained intact. Root verified and removed only this task-created nonsymlink
private `target/debug/incremental` subtree after all diagnostics joined; a new
measurement showed about 755 MB available, still insufficient for a blind full
build. No source or other task's outputs changed. Preserve missing proof as
outstanding and select an available authorized existing local/CI route under
current Evidence Contract/Execution Inputs. No repeated heavy command, global
settings change or other-task cleanup is authorized. The separate #247 repair
has exact-head CI and CodeQL PASS at
`208760df3fc10c721985953abb2b650998881372`.

## Final delivery progress

Current published candidate: `286c220cafbd2aae88509d8432a5d2faed0e5ba4`,
PR [#258](https://github.com/Dankosik/rust-service-template-rest/pull/258).
T4 retains delivery. Source-stage review found one T4 panic-cleanup defect;
its bounded repair is source-ready with no surviving anchored finding. Final
verdict awaits actual required build/tests/R3 and selected CI gates.

Actual CI initializer diagnostic at this candidate found E0277 in the T1 NATS
unit test: `unwrap_err` requires the intentionally non-Debug `Auth` success type
to implement Debug. Root serially returned only that test-file scope to
`/root/credential_followup_counters`; production Debug/auth/counter semantics
must not change. Prior local reviewer joined; remote CI consumes its immutable
old candidate and may continue independent diagnostics. Other writers stay
joined. Root consumed the assertion-only repair and focused compile PASS; file
hash `a8c47d691f40589e420922766eed7e1174f46c88bd5953fc7b51dc0d8e37f849`.

Collected quality diagnostics also required T1/T3 test-local lint corrections
and a T4 cleanup-report lint correction. T1 and T3 original Leads returned their
bounded fixes and released all writers. Root checked `cache/src/tests.rs`
`810d2c0fc71d60777040fbbbf04e959b769c9411311ac1c97e4c7a6620b77a18`
and `cache/tests/support/valkey_auth.rs`
`376788ea278be99d755537911afe9c0e8237c3e27606955ed339c5b17a3caf65`.
Production control flow and runtime oracles are unchanged; final collected lint
belongs to T4 on the replacement candidate.

Actual old-candidate R3 evidence: CI `37497576216`, integration job
`112386110138` succeeded on `286c220`. Valkey named authenticated case passed
within its target's 11 passed/0 failed/0 ignored/0 filtered result. NATS named
expired/old/replacement case passed (1/0/0/0), after the same service entered
authenticated mode; the original service then restored and became healthy.
These are old-head execution results, not replacement-head receipts. T4 owns
equivalence assessment/new-head CI and final independent review/Completion.

Current published replacement `5a84d8376ee2b4050d055e7bbb128cabb0248599`
passed lint/build/workspace tests (943 passed, zero failed, three existing
ignored), both named real R3 cases, restoration and integration proof. Late
quality failed only the newly duplicated T2 test-recorder pair (JWT 1813–1836
and refresh 295–319, 106 tokens). Local duplicate-check reproduced that one
pair; architecture-check passed and its reader/lock joined.

Root routed the original T2 Lead to reuse the existing cfg(test) Diagnostics
recorder, expanding only its test-helper visibility/implementation scope in
`jwt.rs` alongside `refresh.rs`. Other source owners stay frozen. Runtime and
counter/gauge assertions must remain unchanged; no production seam, dependency,
framework or blanket quality policy relaxation. Matching scoped duplicate-check
and fitting unit-test compile feedback precede handoff; T4 retains final
candidate, publication, actual CI and review. Latest capacity reported by T2 is
323 MiB, so no blind build/link or cleanup of other task's outputs is allowed.

T2 clone repair returned Implemented and released both scopes. It reuses the
existing cfg(test) `jwt::tests::Diagnostics` for gauge capture and removes the
copied recorder, net 47 lines removed with assertions preserved. No production
or quality policy changed. Matching duplicate-check passed; focused locked
bearer-auth unit-test compile passed under the shared lock. Root checked files:
`jwt.rs` `75e18601565ba1607c56d654d32df648f897bca6b3fb8009f95085337874cdf7`,
`refresh.rs` `6eced7963c2384fd4e9980e24f827f0f8d8c3757249f581c8e9eb4ad242c146a`.
All repair readers/writers are joined; T4 may freeze the replacement candidate
and continue the existing final review/actual CI boundary. Old-head functional
receipts retain their identities and scopes until equivalence is adjudicated.

## Resume after root interruption

User explicitly continued the same outcome. Root verified clean/synced published
HEAD `df3822e7b39e0c2ef961e358d793acdc969627c2`, ready MERGEABLE PR #258 and
in-progress exact-head CI `37504526810` / CodeQL `37504526832`. The previous
native delivery/reviewer identities are unavailable in this turn. No source
writer remains active. One replacement delivery owner
`/root/credential_followup_delivery_resume` was dispatched through native
`default`, `gpt-6-astra`, `high`, `fork_turns=none`; accepted without rejection.
It owns only the remaining final evidence/review/CI/publication metadata stage;
root still owns this canonical ledger and post-Accepted cleanup.

Earlier source-stage review closed F1 and all mechanical deltas; its latest
bounded helper recheck found no surviving source finding. The unavailable
reviewer requires one fresh bounded final replacement review for the helper
delta and invalidated pending proof, retaining unchanged full-scope reasoning.
No phase, implementation, or already executed unaffected proof is restarted.

## Completion

The resumed delivery owner returned Accepted for
`df3822e7b39e0c2ef961e358d793acdc969627c2`. CI `37504526810` and CodeQL
`37504526832` completed with every selected job and required aggregate SUCCESS.
Workspace Clippy/build, 943 workspace tests (zero failures; three existing
ignored), 90 native NATS tests, both named real-authentication cases and broker
restoration passed. Fresh bounded final reviewer returned PASS/no findings and
joined, retaining the unaffected whole-candidate review. PR #258 is ready and
MERGEABLE; no merge/deployment occurred. Root records the returned verdict
without repeating proof. Durable final evidence is in `completion.md`; the
execution ledger and packets can now be archived in Git and removed.
