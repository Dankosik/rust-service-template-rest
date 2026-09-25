# T1 final delivery validation

Status: Accepted locally for code candidate `0b4bfe0`; PR and exact-head CI remain root-owned.
This is the single delivery/review receipt; the root owns
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

## Projection and diagnostic seam

The clean repaired candidate `3b05fa754cd9add8a11741ae69ead2e0633ce861`
passed `python3 scripts/tests/template-profile-projections.py --source <checkout>`
under the common validation lock: 208 selection/equality records and 22 expected
refusals. Output is retained at `/tmp/authn-projections-3b05fa7.log`. This proves
structural projections and harness parity, not the CI-owned runtime matrix.

The original reviewer closed the ten original source findings except for a
diagnostic integration regression: the adapter and HTTP layer both incremented
the established HTTP outcome counter. The bounded repair preserves
`authn_verifications_total` for one HTTP authentication outcome and names the
engine-reason counter `authn_token_verifications_total`. The existing mounted
TLS test now observes both recorder-visible counts for envelope rejection and
completed verification. Documentation states their distinct scopes.

This metric/test/documentation delta changes no generator, profile selection,
dependency edge, or source marker, so the structural projection evidence remains
reusable for that scope; it is not relabelled as a fresh exact-candidate run.
The mounted count assertion and final review disposition remain pending.

The selected remaining local plan also includes `make secret-scan
BASE_REF=4edd184`: CONTRIBUTING's manifest/lock change rule requires this local
changed-range scan. No full-history scan is selected.

## Core repair execution and requester-scope hold

The mechanical batch on `a896e21` passed formatting, documentation links
(713 successful checks, zero errors) and dependency policy/advisories; reported
duplicate-version warnings were non-failing. It found a four-word skill budget
overrun, four unused dependency edges, and local Clippy issues. Those were
repaired without changing the authentication behavior. The existing HTTP runtime
dependency now explicitly owns its required Tokio-util `rt` feature after the
obsolete authentication fixture dev edges were removed. Registry package
versions/checksums did not change.

Subsequent `make check-instructions` passed all 28 skills, five canonical roles,
the Codex registry and selected Claude/Qwen views. `make unused-deps` passed.
`make lint` passed over the workspace, all targets and the existing integration
feature. Logs: `/tmp/authn-mechanical-repair.log` and
`/tmp/authn-lint-after-bootstrap.log`.

The same bounded `repair_regression` command then exited 0 with all ten selected
tests passing: seven adapter cases, effective public policy, protected startup
refusal with a disabled verifier, and mounted TLS authentication/scope handling
without a Composer. The mounted test observed the separate HTTP/engine counter
totals. No database or live IdP was used; filtered-out binaries are not counted.
Log: `/tmp/authn-repaired-regressions.log`.

These results cover `a896e21` plus the current mechanical code delta. Its SHA-256
is `f2332751d33910f46d513189c95336e1b0b2200a3f89b9ebf6f76311b84cbf96`
for `git diff --binary a896e21 -- .agents/skills/rust-api-contract/SKILL.md
Cargo.lock crates scripts/lib/template_profiles.json test/Cargo.toml`.
This is scoped evidence, not a whole-candidate aggregate receipt.

The canonical local secret-scan wrapper reported a match only in ignored,
generated `.codegraph/codegraph.db-wal`; no raw match is retained here. Gitleaks
directory mode ignores Git exclusions and offers no direct path-exclusion flag.
Root approved the same pinned/redacted/configured directory scan on an exact
`git archive HEAD` export plus the unchanged `4edd184..HEAD` Git-range scan.
Those scans remain pending the final clean candidate; no cache was deleted and
no scanner rule or source pattern was suppressed.

Overall acceptance remains held: requester-meaning reconciliation established
that the original optional-cache instruction requires a delivered opt-in,
default-off feature. Root reopened that narrow Intent/R7/Design/Planning scope.
The corrected cache design and narrow Planning are reviewed. Remaining cache
implementation is authorized by the fixed T1 packet SHA-256
`fe16d7ab6b8a9f40e6a9c5a82f86c5d579a26176ef6c9885da6bbec848efb919`
and was completed by two fresh Astra/xhigh owners for runtime/evidence and
configuration/composition/projections, as recorded below.
The successful core evidence above remains useful only within its unchanged
scope. Full build/workspace runtime validation and final acceptance must cover
the assembled cache-capable result.

## Cache assembly checkpoint

Both fresh cache writers returned Implemented and joined. Cache source, private
verified temporal evidence/allocation accounting, introspection-only config and
bootstrap conversion, operator guidance, existing caller migration and projection
assertions are assembled. A shipped local-config loader regression accompanies
the correction of dormant introspection fields under `mode = "none"`.

The bounded compile-only command for all six affected production/test packages
and the integration feature passed with no diagnostics; Python parsed the changed
projection assertions. These are static results, not cache behavior evidence.
`make fmt` completed and canonical `make openapi-generate` completed without a
YAML delta. No cache runtime tests, lint or final integrated review ran during
coding. The root's next clean commit will own the assembled validation inputs.

## Complete-candidate mechanical review

The complete candidate was committed as
`73e08a1c3af75de87ecf08603a8861e0dd0703fc`. Its formatting, instruction/carrier,
documentation, unused-dependency and dependency-policy checks passed. The only
lint failure was the startup-only `PreparedAuth` enum's enlarged inline verifier.
After the fresh reviewer joined, that private variant was boxed once during
preparation and consumed into the unchanged HTTP finalizer; profile markers and
runtime policy did not change. The focused workspace lint rerun passed, including
all targets and the integration feature. Logs are
`/tmp/authn-complete-mechanical-73e08a1.log` and
`/tmp/authn-lint-box-repair.log`.

A fresh Astra/xhigh cache/integration reviewer independently inspected the full
new cache boundary and its affected integration, consuming prior core closure
evidence. It found no additional behavioral defect; its FAIL was limited to the
actual lint failure above. The same reader will check that bounded representation
delta and receive the remaining runtime/projection/security evidence before a
final review disposition.

## Runtime results and bounded repair

`make build` passed on clean `ac2bc09c6a7ce6023e041926f1f18e4e760cde2a`.
The ordinary workspace run used `make test CARGO_FLAGS="--locked --no-fail-fast"`
and completed all targets: 461 passed, two failed, none ignored. Its log is
`/tmp/authn-workspace-tests-ac2bc09.log`; the build log is
`/tmp/authn-build-ac2bc09.log`. This failed aggregate is not relabelled as passing.

One failure was real environment integration: internally tagged serde buffering
lost config-rs scalar conversion for cache controls. A private field-local
adapter now accepts their typed TOML forms or exact scalar strings without
coercing audience, client or credential strings. The complete config library
rerun passed all 101 tests, including invalid values and numeric/boolean-looking
string preservation (`/tmp/authn-failed-targets-diagnosis.log`).

The diagnostic test's first failure was a fixture error: jsonwebtoken's Other
key-family fallback accepts the old `kty: false` shape, so it correctly emitted
100 unsupported-family rejections. A wrongly typed consumed `use` member now
exercises the intended malformed-entry branch while retaining the private-data
sentinel and all original assertions. Its subsequent isolated-pass/parallel-fail
trace capture was independently diagnosed against tracing-core 0.1.36: the
single-dispatch callsite optimization consults the registering sibling thread's
default. The test now retains a second independent, non-default NoSubscriber
dispatch, so merged interest performs the correct scoped lookup. No production
observer, global subscriber or suite serialization was added. One normal
parallel adapter rerun then passed all 33 tests
(`/tmp/authn-adapter-anchor-repair.log`).

The other 330 successful workspace tests are retained within their unchanged
scope; the two failed library targets have their passing scoped results above.
The cache-specific TLS, controlled-clock, capacity, payload, contention, expiry,
context-isolation and unsuccessful-result cases actually executed and passed.
The current-cache mounted TLS/no-Composer selector additionally passed one test
with 21 intentionally filtered, checking actual authorization and distinct
HTTP/engine recorder counts (`/tmp/authn-mounted-current-cache.log`). It started
no database. Filtered tests are not counted as passes.

After these bounded repairs, workspace lint again passed for all targets and the
integration feature (`/tmp/authn-lint-final-repair.log`). Matching build-delta,
current structural projections, exact-tree/range redacted secret scans, and the
reviewer's final evidence disposition remain pending the next clean checkpoint.

## Final local disposition

Code candidate: `0b4bfe099381f140afb2d33886b09db958e02ba3`.
The fresh cache/integration reviewer returned **PASS**, with no surviving
findings, after consuming the repaired scoped test results and final receipts.
All implementation, repair, diagnosis and review descendants are joined.

Final `make build` passed (`/tmp/authn-build-0b4bfe0.log`). Current structural
projections passed 208 selections/equalities and 22 expected refusals
(`/tmp/authn-projections-0b4bfe0.log`). Gitleaks 8.30.1 with unchanged config
SHA-256 `1b08e8ef509a65ca68a3967095e54553fb9ec097ec972399574bfc55620d3e04`
reported zero leaks for both the exact tracked HEAD export (tree
`4134b9c25d762df46a42a721f67e87d772347678`) and the seven-commit
`4edd184..0b4bfe0` range (`/tmp/authn-secret-scans-0b4bfe0.log`). Both checks used
redaction; the local CodeGraph cache and scanner policy were unchanged.

Local acceptance uses the passing matching build, repaired config/adapter/mounted
test receipts, unchanged workspace passes, selected static/security checks and
independent review. The earlier failed workspace aggregate is not promoted to a
passing aggregate receipt. The 26 generated runtime variants and database proof
remain CI-owned, and no live IdP or deployment was certified. Root owns the
remaining separate PR, exact-head CI and final publication closeout.

Final receipt `make docs-check` passed 861 link checks with zero errors
(`/tmp/authn-final-receipt-docs.log`). This status-only closeout adds no links or
changes to the validated source, link targets, or profile inputs.
