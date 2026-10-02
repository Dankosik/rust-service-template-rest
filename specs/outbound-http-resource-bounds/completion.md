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

Pending final validation and one fresh independent assembled delivery review.
