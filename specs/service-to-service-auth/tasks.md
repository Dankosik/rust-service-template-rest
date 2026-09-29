# Tasks

Each task owns disjoint files, follows [design](design/system.md), and proves
its [spec](spec.md) items with `make build` plus its crate tests. Lanes do not
commit; the coordinator reviews, integrates and commits.

| ID | Outcome | Files | Depends | Status |
| --- | --- | --- | --- | --- |
| T1 | B1, B2: private-key assertions, form POST without `oauth2`, token exchange with cache, `OnBehalfOf` in HTTP and gRPC bindings, tests | `crates/infra-oauth2-client-credentials/**`, root `Cargo.toml` (oauth2 removal, uuid `v4` if needed), `Cargo.lock` | — | pending |
| T2 | B1 configuration keys and validation | `crates/config/src/{integrations,load,lib}.rs` and their tests | — | pending |
| T3 | B3, B4: `access_token()`, `actor()`, OpenAPI scope enforcement, `require_scope` removal | `crates/infra-bearerauthn/**`, `crates/infra-http/src/{authn,contract,lib}.rs`, `crates/service/tests/openapi.rs`, `test/tests/http_idempotency/mounted.rs` | — | pending |
| T4 | B5 and initializer: new guide, decision record, authentication/outbound guides, roadmap, README, other docs naming `client_secret`/`require_scope`, profile inventory, lock feature edges, initializer tests | `docs/**`, `README.md`, `scripts/lib/**`, `scripts/tests/**` | T1 (lock edges) | pending |
| T5 | Delete this bundle after its durable content has moved; final validation, PR, CI | `specs/service-to-service-auth/` | T1–T4 | pending |
