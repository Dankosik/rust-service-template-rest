# TA — `SERVICE_AUTH` profile

Outcome:
A template initialized with `SERVICE_AUTH=jwt` and `GRPC=enabled` verifies a
caller-signed ES256 token on every gRPC business call, admits callers per
method from the receiver's table, hands the handler `Caller`, signs outbound
tokens, and keeps every existing profile combination building and passing;
`SERVICE_AUTH=none` (default) removes all of it.

Consumes:
- [spec B1–B7](../spec.md) — behaviour, runtime seam, statuses, metrics.
- [decision record](../../../docs/service-authentication-decisions.md) — S1–S13, crates, placement.
- `scripts/lib/template_init.py` and commit 34982f4e — the pattern for adding a profile.

Provides:
- Crate `crates/infra-serviceauth`; configuration section `service_auth`;
  `Services::allow_callers`, `Services::authenticate_callers`,
  `Client::with_service_tokens`, `infra_grpc::Caller`; `AppState.service_tokens`.
- Initializer input `SERVICE_AUTH`, lock field `service_auth`, inventory key
  `service-auth`, `jsonwebtoken` selection and Cargo.lock feature-edge rules.
- Guide `docs/service-authentication.md`, decision record, amended mandate,
  gRPC guide, configuration policy, template sync, boundaries, roadmap entry.

Boundary:
gRPC inbound and the transport-neutral token source. Bearer gRPC stays the
choice whenever the process verifies no service token; existing tests that
build routers without rules or a verifier need no change. The gRPC example
declares rules under the profile marker. No HTTP inbound service tokens, no
Kubernetes adapters, no production trust registry or rotation tooling.

Mutable owners:
- `infra-serviceauth`, `infra-grpc` (router, admission, client binding, errors), `service-config`, `service` bootstrap and state, gRPC example.
- Initializer, lock reader, profile inventory, projections, initializer matrix (graph narrowing by path), sync and safety tests; `scripts/ci/changed-surfaces.sh` classification of `crates/infra-serviceauth/*` like `crates/infra-bearerauthn/*`.
- `quality/architecture.json` roles; duplication baseline only if the checker demands it.
- Documentation listed under Provides.

Exclusive locks:
- `scripts/lib/template_profiles.json`, `Cargo.lock`, `Cargo.toml` workspace dependencies, `crates/infra-grpc/src/router.rs` public API.

Final validation:
- Claim: the contract refusals (no token, foreign audience, expired, another issuer's key, method outside the table), the startup refusal without a rule, no token text in logs or spans, and the success path hold; every projected combination still builds; the profile is removed cleanly by `none`.
- Checks: workspace `make build`, `make test`, `make lint`, `make fmt-check`; `make duplication-check`, `make architecture-check`; `make template-init-projections`; initialized runtime graphs with `SERVICE_AUTH=jwt` without `AUTHN` and with `AUTHN=oidc-jwt` (both Cargo.lock edge rules) through their tests; a bootstrap-level test where a `mode = jwt` process refuses a tokenless gRPC call; `make docs-check`.
- Observable: test results and projection records.

Reopen if:
The runtime seam (S4) cannot keep the source template's bearer gRPC tests and the service-token tests passing together.
