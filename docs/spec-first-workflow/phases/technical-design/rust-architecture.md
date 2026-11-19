# Rust Architecture And Ownership

Disclosed branch of [Technical Design](../technical-design.md). Own semantic
crate/module ownership and cross-seam Rust contracts using `rust-idiomatic`
(invariants, representation, ownership, lifetimes, traits) and
`rust-structural-quality` (deletion test, polymorphism, co-change) across
callers, composition roots (`crates/service`, `crates/jobs-worker`,
`crates/migrate`), generated contracts, and replacement paths.

## Vocabulary And Deep-Module Discipline

Use consistent design language in `design.md`: **module** (scale-agnostic unit
with an interface and implementation), **interface** (everything a caller must
know: types, methods, invariants, ordering, error modes, lifetimes, cost),
**seam** (where an interface lives), **adapter** (concrete implementation at a
seam), **depth / leverage** (complex logic hidden per unit of interface
learned), and **locality** (change, bugs, and verification concentrated in one
place).

- **Collapse shallow chains:** eliminate pass-through layers (`handler ->
  service -> repo -> mapper`) that scatter reasoning without owning a distinct
  invariant.
- **Caller usage walkthrough:** for each open ownership choice or non-trivial
  new/changed seam, write a 3–8 line realistic caller sketch against real
  workspace types (`AppState`, `OperationContext`, `service_failure::Code`,
  `infra_http::Problem`, `&mut Tx<'_>`). Verify caller obligations, invariant
  enforcement, and co-change;
  retain a proposed crate, module, trait, wrapper, or helper only when deleting
  it scatters an invariant across callers or violates a dependency-direction,
  compile-time, or lifecycle constraint.
- **Seam discipline & test surface:** *one adapter is a hypothetical seam; two
  real adapters (or a true external seam) make a real seam.* Shape interfaces
  so Implementation tests observable behavior through the seam without exposing
  `pub` internals.

## Dependency Categories

1. **Pure / in-process** (domain logic, state machines, codecs,
   `crates/shared-rules`): no port or trait; absorb into a deep module and test
   through its interface.
2. **Local-substitutable / real workspace backend** (`infra-postgres`,
   `infra-cache`, `infra-messaging`, `infra-object-storage`, Axum `Router` via
   `tower::ServiceExt`): keep the seam internal or at the transport interface;
   test against the real local store or router instead of a mock-only trait.
3. **Remote / true external seam** (`infra-outbound-http`,
   `infra-outbound-grpc`, OIDC, webhooks): use the real client adapter against
   a local wire fixture (loopback HTTPS / `wiremock`), or a minimal port/trait
   only when ≥2 real adapters exist or pure orchestration must verify
   independently of transport.

Use [Design It Twice](design-it-twice.md) for hard-to-reverse cross-crate forks.

## Rust Contracts, Compile Reality, And Workspace Graph

- **Unrepresentable invalid states:** parse/validate at the owning seam and
  carry checked values inward, encoding ordering, validated capabilities,
  transaction scope, and resource ownership in Rust types, enums, and lifetimes
  (auditing `Deserialize`, `From`/`TryFrom`, `Default`, and field visibility).
- **Static compile-reality check:** verify every proposed signature and caller
  sketch against inspected code, `Cargo.lock` sources, workspace `Cargo.toml`
  lints (`unwrap_used`, `expect_used`, `panic`, `unsafe_code = "forbid"`),
  `clippy.toml` bans (direct `std` blocking I/O, `tracing::span::Entered`
  across `.await`, raw `sqlx` pool `begin`/`acquire`, raw `axum`
  extractors/routing), and patched `vendor/` forks: `Send + Sync + 'static`
  across `.await` (no non-`Send` guards held across `.await`), `dyn` object
  safety (prefer concrete types, closed enums, or generics), `&mut Tx<'_>` and
  `OperationContext` lifetimes, `axum::extract::FromRequestParts` vs
  body-consuming `FromRequest` order, and `sqlx::query!` / `utoipa`
  constraints.
- **Workspace graph, allowlists & profiles:** reconcile crate edges against
  [`quality/architecture.json`](../../../../quality/architecture.json)
  (including `allow_dependents` on `crates/shared-rules`,
  `crates/operation-context`, and `crates/json-payload`) and [Repository
  Architecture](../../../repo-architecture.md#global-invariants): feature
  crates avoid unapproved transport/provider edges; `crates/shared-rules` stays
  `std`-only; optional profiles stay independent, inert unless selected, and
  registered in `scripts/template/profiles.json` when adding profile paths;
  generated contracts (`api/openapi/service.yaml`, `crates/grpc-contracts`,
  `.sqlx/`, harness files) keep one canonical writer. Name all superseded code,
  config, contracts, or tests to delete in the same change.

Record results in [Design V1](../../interfaces/design-v1.md).
