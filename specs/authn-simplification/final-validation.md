# T1 final delivery validation

Status: in progress. This is the single delivery/review receipt; the root owns
the canonical ledger and publication. Local acceptance and requested PR/CI
completion remain distinct.

## Candidate and plan

The first fixed implementation candidate was
`aa5353e2950abd82c53f88cec074cc70c3ad9912`, against production base `4edd184`.
`make plan` selected formatting, lint, build, workspace tests, unused dependency
and advisory checks, instructions and documentation. The initializer runtime
matrix and database integration are CI-owned. No local full matrix, live IdP
or new database environment is selected.

The initial mechanical batch was still waiting for the common validation lock
when review found a defect. Its own waiting process was terminated (exit 143),
with no child reader remaining and the sibling's lock unchanged. No check from
that cancelled batch supplies passing evidence.

## Independent review and repair scope

A fresh native Astra/xhigh `final_review` performed the complete read-only
Implementation Review on the first candidate and returned FAIL. Its ten
anchored findings covered:

1. Missing `invalid_request` challenge for malformed Authorization.
2. Unsupported authentication schemes classified as malformed.
3. Missing active introspection claims/identity classified as provider failure.
4. Coherent alias-only JWT client identity rejected.
5. Supplied empty and nonempty scope forms accepted despite disagreement.
6. Broken nested/repeated/obsolete profile markers and retained disabled-engine
   references, including removal of authentication finalization with idempotency.
7. Missing safe preparation context and closed diagnostic distinctions/reasons.
8. Refresh waiters incorrectly capped by the provider budget and late completion
   taking precedence over the waiter's expired deadline.
9. Unconfigured JWT algorithms reaching refresh before local rejection.
10. Protected finalization accepting a disabled verifier.

Two fresh Astra/xhigh repair writers own the adapter and HTTP/bootstrap/profile
surfaces. Diagnosis also found that optional security-decision context was
required, absent root/operation security was rejected instead of public, and
inactive introspection envelopes ignored duplicate consumed fields. These are
repairs to the same accepted policy and typed-envelope contracts; no requirement
was relaxed and no Specification or Planning reopening was needed.

The original reviewer remains the owner of the bounded repaired-candidate
recheck. A positive review disposition is pending.

## Baseline falsifier

Before production repair, both writers added focused regression cases and paused
all writes. Under the common validation lock, this command actually executed
the selected cases:

```text
cargo test --locked -p infra-bearerauthn -p infra-http -p service \
  -p integration-tests --features integration-tests/integration \
  --no-fail-fast repair_regression
```

Result: exit 101; 10 selected tests failed on their intended observable:
seven adapter identity/classification/scope/algorithm/deadline cases, one public
policy case, one service disabled-verifier finalization case, and the mounted
TLS/no-Composer authorization case. The last reached an actual HTTP request and
failed because `WWW-Authenticate` was absent. The signed-JWT case used the
existing signing fixture. No database test or live provider request was selected.
Unselected binaries reporting zero tests are not counted as proof.

Raw local output: `/tmp/authn-baseline-regressions.log`. These are baseline
failure results, not acceptance evidence. Passing repaired scenarios and the
remaining selected local plan are pending.

## Repaired source checkpoint

Both raised repair writers completed their production/test changes and froze
their scopes. HTTP now shares effective-policy classification across both
finalizers, refuses disabled protected composition, supplies the malformed
challenge, and retains auth finalization independently of idempotency. The
adapter repairs the typed claim and scheme classifications, scopes/aliases,
algorithm admission, independent waiter deadlines, and safe contextual/closed
diagnostics. Shared metrics remain available in introspection-only outputs.

Marker coverage was reconciled with the actual source; no generator rewriting
or new validation infrastructure was introduced. In the affected config,
bootstrap, adapter and HTTP-auth source files, auth marker pairs decreased from
97 at production base to 70. JWT, introspection and refresh engines are excluded
as whole files; conditional markers remain only where shared surfaces need them.
The whole-project marker inventory count is not an auth complexity target.

Formatting is authored. The root will create a clean repaired commit before
the existing HEAD-based projection checker runs. Repaired execution and the
original reviewer's bounded delta disposition remain pending.
