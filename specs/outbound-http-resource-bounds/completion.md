# Outbound resource bounds: execution and Completion

## Assembled implementation

The single [accepted unit](implementation-unit.md) is implemented across the
transport, OAuth, configuration, and documentation owners. All three disjoint
implementation writers joined before final validation. The Lead owns the
assembled candidate, repair, review, and local Completion; the continuation
coordinator owns publication of one PR. No merge or deployment is authorized.

The collector copies and releases each frame, with capped geometric requested
storage. OAuth shares immediate provider admission across both grants, with
capacity held through parsing and token admission. Configuration defaults to
32 with strict scalar/range validation; direct options, gRPC projection,
metrics, examples, and resource-bound/replay documentation are aligned.

Test selection reused existing authorization/cache/EOF/deadline/observation
coverage and added fragmented exact-limit/trailer/late-error cases, real-loader
scalar/source boundaries, mixed-grant overload and hit/waiter behavior,
cancellation release, background-refresh refusal, and sanitized gRPC refusal
before resource dispatch. No production seam or dependency was added for tests.

## Selected final validation

The mixed-surface plan uses base `67be869acea112af271ec8ba621cbc50ae9d36b7`:
matching `make build`, followed by the local steps of `make verify`:
`unused-deps`, `fmt-check`, changed-owner `lint-changed`, affected-closure
`test-changed`, and `docs-check`. The affected tests cover infra-grpc,
infra-http, infra-oauth2-client-credentials, infra-outbound-http,
infra-webhooks, integration-tests, jobs-worker, migrate, service, and
service-config. The clippy variant includes the OAuth integration feature.
The shared validation lock serializes CPU-heavy work. Cargo runs locked with
the pinned toolchain and existing primary-checkout target directory, with no
cache clearing. Initial observation checkpoint: 30 seconds, then progress-based
checkpoints from compiler/test output.

CI owns `make template-init-check` and `make test-integration-oauth`; no
`ALLOW_FULL` or `ALLOW_HEAVY` expansion is selected. Local completion does not
claim live-provider, CI, merge, deployment, RSS, or throughput evidence.

## Completion result

```text
unit: Completion
verdict: Accepted
candidate: 945cbbfa4252991dfff61ff59103e7c1f3a60cff
review: PASS, no findings; review-result.md
next_owner: Coordinator publishes final head to PR #223 and owns CI
```

All code writers and validation readers joined. The final production, test,
configuration, and guide candidate is fixed. Later closeout edits only record
this evidence; they do not change checked source, manifests, or dependencies.

## Actual evidence

Local environment: Darwin 25.4.0 arm64, pinned Rust 1.99.0 after integration,
Cargo 1.99.0, Docker 29.4.0. Commands used the task checkout, PATH including
`/Users/daniil/.cargo/bin` and `/opt/homebrew/bin`, and
`CARGO_TARGET_DIR=/Users/daniil/Projects/Opensource/rust-service-template-rest/target`.
Cargo commands used `--locked`. Build and scoped continuations ran under
`scripts/ci/validation-lock.sh`; `make verify` takes that same lock itself.
Contending repository validation was allowed to finish without interruption.

| Claim | Actual command and scope | Result |
| --- | --- | --- |
| Matching deliverables compile on current main's compiler | `make build CARGO=/Users/daniil/.cargo/bin/cargo` on merge candidate `5b927b14a70ca377fb5bc98f04093bcfb5b906b1` | PASS, 12.30s; later code delta is one equivalent test assertion |
| Formatting | `make fmt-check` on `945cbbf` | PASS |
| Changed-owner lint, including OAuth's integration-feature code | `make lint-changed PKGS="infra-oauth2-client-credentials infra-outbound-http service-config"` on `945cbbf` | PASS, 35.64s |
| Affected behavior and caller compatibility | `make test-changed PKGS="infra-grpc infra-http infra-oauth2-client-credentials infra-outbound-http infra-webhooks integration-tests jobs-worker migrate service service-config"` on `945cbbf` | PASS, 527 tests across 41 result groups, zero failures or ignored tests; includes 51 OAuth and 23 outbound tests |
| Dependency-use consistency | `make unused-deps`, from the original `make verify` attempt on `46f9def` | PASS; dependency declarations and their users are unchanged by subsequent test-only repairs and upstream compiler integration |
| Guide and task relative links/fragments | `make docs-check` on the final evidence-record closeout | PASS, 866 links, zero errors |
| Accumulator retention and requested growth, permit lifetime, observable/error/config/caller consistency | Fresh independent assembled delivery review and bounded integration recheck | PASS, no findings; [receipt](review-result.md) |

Machine-local logs: `/tmp/outbound-resource-bounds-rust199.log` (matching
build and initial new-compiler lint diagnostic),
`/tmp/outbound-resource-bounds-rust199-lint.log`,
`/tmp/outbound-resource-bounds-rust199-tests.log`,
`/tmp/outbound-resource-bounds-verify.log`, and
`/tmp/outbound-resource-bounds-continuation.log`, and
`/tmp/outbound-resource-bounds-docs-final.log`. The final documentation run
first found the existing Docker daemon stopped; starting the existing OrbStack
runtime restored Docker 29.4.0, and the same pinned offline check passed. No
new environment was created. The subsequent status-text update changes no
links or fragments, so that link proof remains applicable.

The first `make verify` on `46f9def` passed unused-deps and formatting, then
failed Clippy because the extended existing admission table exceeded 100 lines.
A documented site-local allowance preserves the single table. Its focused
continuation passed lint, all 527 tests, and documentation on `f512493` under
Rust 1.98.1. The actual failed aggregate attempt is retained at
`/Users/daniil/Projects/Opensource/rust-service-template-rest/.git/codex/verify/attempt-6c2d80d13183.eSOdk9`.
No successful aggregate receipt is fabricated: Completion uses those valid
scoped results and the actual passing continuations.

After readers joined, upstream `546a381aa74286bce5336b5b59c7bf46bf6f3bae`
(Rust 1.99 upgrade, already merged through PR #218) was merged cleanly as
`5b927b1`. This task adds no dependency or toolchain change relative to that
current main. The compiler change invalidated the earlier compiler-based proof,
which was rerun. Rust 1.99 identified one new diagnostic assertion rewrite;
`945cbbf` replaces `assert!(!before_capacity.is_empty())` with an equivalent
`assert_ne!`, without suppressing the lint. The build remains applicable across
that test-only rewrite; failed lint and pending tests were rerun and passed.
The current `make plan BASE_REF=546a381aa74286bce5336b5b59c7bf46bf6f3bae`
retains the same task-local route and CI-owned remainder.

## Delivery boundary

[PR #223](https://github.com/Dankosik/rust-service-template-rest/pull/223) is
owned by the continuation coordinator. The coordinator pushes the closeout head
and marks the same draft ready after this local Completion, then records actual
CI. `make template-init-check` and `make test-integration-oauth` remain CI-owned;
local tests do not certify a live OAuth provider. No merge or deployment has
been performed or authorized. No process-RSS or performance measurement is claimed.
