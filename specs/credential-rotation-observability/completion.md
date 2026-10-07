# Credential rotation observability: Completion

Status: done. Delivery verdict: **Accepted**. Final independent review:
**PASS**, no findings.

Authority: [Intent](intent.md), [Specification](spec.md) and
[Design](design/design.md). [PR #258](https://github.com/Dankosik/rust-service-template-rest/pull/258)
names these durable decisions and this evidence. Git archives the execution-only
ledger and packets; they are removed at closeout.

## Fixed candidate and execution inputs

Base: `699887b18594088a59bcc23a049d290d089f6da1`.
Accepted source candidate: `df3822e7b39e0c2ef961e358d793acdc969627c2`.
CI executed PR merge `b3d0d6b2aae368e15ced40c4d9ada426ed17c8fe`, with the
expected base/head parents and tree
`d657c64622a447e681d1410410afc32543e21268`, identical to the candidate tree.
Later execution-artifact cleanup changes documentation only; it does not
relabel these runs as fresh cleanup-head execution.

## Actual verification

| Result | Evidence and exercised scope |
| --- | --- |
| CI: SUCCESS | [37504526810](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37504526810): every selected job and `required` passed. |
| CodeQL: SUCCESS | [37504526832](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37504526832): Rust, Actions and `codeql-required` passed. |
| Workspace quality | [Job 112409733580](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37504526810/job/112409733580): Clippy/build passed; **943 tests passed, zero failed, three existing ignored, zero filtered**; separate native NATS **90 passed**. Duplication and architecture passed. |
| Real authentication | [Job 112409733961](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37504526810/job/112409733961): both required cases executed; ordinary NATS consumers finished before authenticated configuration, and normal broker configuration restored healthy. |
| Other selected gates | Documentation, secrets, security, delivery, OAuth, runtime progress and selected initializer runtime/projection graphs passed in the same CI run. |

Valkey case
`valkey_auth::replacement_password_is_authenticated_on_retained_and_new_connections`
passed in its target's **11 passed / 0 failed / 0 ignored / 0 filtered**, 10.23 s.
It observes actual server AUTH replies on retained and new adapter connections,
rejection without acceptance signals, pending-password recovery and old-password
fresh-authentication refusal; state belongs to one synthetic ACL user.

NATS case
`expired_old_credentials_are_refused_and_file_replacement_recovers_the_client`
passed: **1 / 0 / 0 / 0**, 8.66 s. The broker validated the operator/account/user
chain and nonce signature, refused expired old material, and the existing
client recovered after atomic replacement. The original broker became healthy
again at 17:42:40 UTC before the runner returned success. This is fixture proof,
not a production rotation, revocation deadline or zero-downtime claim.

Three ignored workspace cases retain their pre-existing owners: actual Go-wire
generation is exercised by integration, one child fixture by its parent test,
and Docker release-image proof is a separately selected scope. They are not
reported as workspace passes.

Evidence log hashes: quality
`b2eafbdbd8a29a50bda3522f42b1d1dc490b81d7aecdc908dcab4c3806a7ba38`;
integration `4d1d554d133aa27ba50793601764c7f133dd4ae0adfd7bc20c233289441b6f40`.
The existing CI retains the source logs and selected-command results.

## Final review

Fresh reviewer `/root/credential_followup_delivery_resume/final_delta_review`
used native reviewer/Astra/high with no inherited turns. Verdict: **PASS**;
findings: none; reopen owner: none. The reviewer joined before closeout.

It independently checked `5a84d837..df3822e`: test-recorder consolidation stays
under `cfg(test)` and preserves gauge/name/label, acquisition and cancellation
assertions, with no production behavior change. Fresh quality and integration
logs establish actual affected tests and both authentication cases. Required CI
and CodeQL receipts and executed-tree equivalence close the pending evidence.
Unchanged prior whole-candidate review and its NATS panic-cleanup finding F1
remain valid; F1 was independently closed. Root ledger annotations are excluded
from the source review. Root records this returned verdict without repeating it.

## Local scope and repairs

The workstation's Docker socket was absent and free disk below 1 GiB, so no
local consolidated build/link or container run is claimed. Matching build,
workspace tests and R3 execution came from the existing CI jobs; no replacement
environment, runner or infrastructure was provisioned. Local formatting,
whitespace, pinned Actionlint/Gitleaks, host ShellCheck, offline Zizmor,
dependency policy, unused-dependency and template-purity checks passed. Narrow
compile/Clippy/duplicate-check feedback is retained only at its exercised scope.
Root removed only this task-created nonsymlink private incremental subtree
after diagnostics joined; shared caches and other tasks' outputs were untouched.

CI/source review exposed and closed four bounded kinds of issue: panic-safe
NATS fixture cleanup; a test-only `Auth: Debug` requirement; test-local lint
mechanics; and a copied recorder. The final recorder reuses existing
`jwt::tests::Diagnostics`, removing a net 47 test lines with assertions intact.
No quality threshold, broad baseline or production policy was weakened.

## Synthetic trust provenance

Fixed synthetic trust was authored once with `/Users/daniil/.local/bin/nsc`,
reporting `nsc version 0.0.0-dev`; executable SHA256
`1596e234feb715568948f6f91dd29c9756a58c1961aa06ea179dccb98a716b8a`.
Every invocation used `-H` with an isolated task-created temporary store. No
real store/account/credentials or global machine configuration was touched.
The temporary store and operator/system private keys were removed; only public
synthetic trust and the synthetic application signing input remain as fixtures.
Runtime tests use existing `nkeys`, `serde_json` and `base64`; nsc has no CI or
runtime role. Gitleaks' exception selects only rule `jwt` and exact public
fixture path `env/nats/credential-rotation.conf`, within profile removal.

No merge, deployment, real credential change, TLS reload or stale-key policy
was performed. Reopen the owning specification/design only for a changed signal,
authentication policy, source/library contract or fixture lifecycle guarantee.
