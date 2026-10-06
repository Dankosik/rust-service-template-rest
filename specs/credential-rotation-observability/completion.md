# Credential rotation observability: delivery evidence

State: implementation complete; final verification and independent review pending.
The single delivery owner is the T4 Lead after root assembled T1–T4 and joined
all writers. [Tasks](tasks.md) remains the canonical root-owned ledger.

## Candidate and accepted scope

Base: `699887b18594088a59bcc23a049d290d089f6da1`. The upstream main readback at
publication preparation still equals this base. This separate delivery follows
[Intent](intent.md), [Specification](spec.md), and [Design](design/design.md).
PR #247 is neither a dependency nor a proof source for this candidate. No merge,
deployment, real credential, live service or global machine change is included.

## Consolidated verification route

The messaging manifest registration selects the broad Rust route: matching
workspace build and workspace tests. The existing CI quality job executes
`make --keep-going lint build test` for this manifest surface. Existing jobs
also own the selected profile projections, security/workflow/docs checks and
real provider suites; no new environment, runner or matrix was added.
The integration job must execute both the Valkey authenticated fixture and
NATS `credential_rotation`, after the ordinary anonymous broker consumers
finish, and restore normal NATS configuration before reporting success.
Compile-only, skipped, filtered-away and zero-test results cannot satisfy R3.

The workstation cannot run this full route: Docker's configured OrbStack socket
is absent, and the refreshed free disk was about 751 MiB. Full build/link and
container execution were not attempted. No shared cache or other task's target
was cleared; only root's previously recorded task-private incremental cleanup
occurred. CI execution at the exact published candidate must supply the pending
build, unit and R3 evidence. This is not a local full-validation receipt.

## Local results

| Command / scope | Actual result |
| --- | --- |
| `make plan BASE_REF=699887b18594088a59bcc23a049d290d089f6da1` | Complete; all changed paths classified, workspace build/test route and existing integration/profile jobs selected. |
| `make fmt-check` | Passed on assembled source. |
| `git diff --check` | Passed before publication preparation. |
| `make actionlint ACTIONLINT=actionlint` | Passed; host Actionlint 1.7.12 matches the repository pin. |
| `shellcheck -x scripts/ci/test-integration-messaging.sh` | Passed; host ShellCheck 0.11.0 matches the container version. Pinned-container gate remains CI-owned because local Docker is unavailable. |
| `make secret-scan GITLEAKS=gitleaks BASE_REF=699887b18594088a59bcc23a049d290d089f6da1` | Passed after the exact fixture-path JWT allowlist. Host Gitleaks 8.30.1 matches the pin. The working-tree pass covers new fixtures; the pre-commit range contained no commits. |
| `bash scripts/ci/validation-lock.sh -- make deny unused-deps` | Passed. Existing duplicate-dependency warnings and an existing redundant `rcgen` ignore are nonfatal; dependencies and lockfile are unchanged. |
| `make zizmor` | Passed in offline mode; online-only audits remain CI-owned. |
| `make template-owned-purity-check` | Passed; profile runtime/projection gates remain CI-owned. |
| T2/T3/T4 bounded `cargo check --locked` feedback | Passed at the recorded unit scopes only; not runtime proof. |

Any later source repair invalidates only the checks and review reasoning that
consume it; pending CI will be bound to the replacement commit. Exact run/job
locators, exercised test names and final review disposition are recorded at
completion, not inferred from this plan.

## Integrated review and repair

Fresh independent reviewer `credential_followup_nats_delivery/integrated_review`
(native reviewer, Astra/high, fresh history) reviewed `dcbea6e` against the
accepted whole-spec boundary. Its first source-stage result found one T4 defect:
a panic after NATS stream creation bypassed cleanup for supported unmanaged
endpoints. No other anchored source defect was found; runtime proof was pending.

The bounded repair catches scenario panics, retains the adapter outside that
scope, completes bounded client close, stream deletion and administrator drain,
then resumes the original panic with cleanup status. It covers creation errors
as well as later assertions. Same-reviewer recheck and matching CI execution
remain pending; no earlier unit result is relabelled as final acceptance.

## Synthetic trust provenance

The fixed operator, system account and application account were created once
with `/Users/daniil/.local/bin/nsc`, reporting `nsc version 0.0.0-dev`; executable
SHA256 `1596e234feb715568948f6f91dd29c9756a58c1961aa06ea179dccb98a716b8a`.
Every invocation used `-H /tmp/nats-rotation-assets.zF7Wig`, an isolated
task-created store. No real nsc store, ambient account, real credentials or
global machine configuration was touched. Only public synthetic trust claims
and the synthetic application-account signing seed entered the repository.
The temporary store was removed after extraction; operator/system private keys
are not retained. The tool has no runtime or CI role: the target issues its
short-lived nonce-bearing users with existing `nkeys`, `serde_json` and `base64`.

Gitleaks found exactly the three public JWT trust claims in
`env/nats/credential-rotation.conf`. The added exception selects only rule `jwt`
and that exact path inside the existing messaging-removal marker. The synthetic
seed produced no scanner finding and received no broader exception.
