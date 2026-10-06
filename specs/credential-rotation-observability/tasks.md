# Credential rotation observability delivery

status: ready

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
scopes released. The T4 Lead now owns the single final delivery boundary:

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
