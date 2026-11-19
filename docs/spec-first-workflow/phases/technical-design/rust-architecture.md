# Rust Architecture And Ownership

Disclosed branch of [Technical Design](../technical-design.md). Own semantic
crate/module ownership and cross-seam Rust contracts under `rust-idiomatic` and
`rust-structural-quality` across callers, composition roots (`crates/service`,
`crates/jobs-worker`), the SQL migrator (`crates/migrate`), generated
contracts, and removals.

## Vocabulary And Deep-Module Discipline

Speak consistently in `design.md`: **module** (unit with interface and
implementation), **interface** (everything a caller must know: types, methods,
invariants, ordering, error modes, lifetimes, cost), **seam** (where an
interface lives), **adapter** (concrete implementation at a seam), **depth /
leverage** (logic hidden per unit of interface), and **locality** (change,
bugs, and verification in one place).

- **Depth belongs to the interface, not the implementation:** a deep module may
  compose private helpers or internal seams without `pub` leaks. Collapse
  shallow pass-through chains (`handler -> service -> repo -> mapper`) that add
  no invariant.
- **Highest seam & caller walkthrough:** prefer existing seams over new ones
  and place seams at the highest observable point hiding internal wiring. For
  each non-trivial new or changed seam, write a compact Rust signature and 3–8
  line caller sketch against workspace types (`AppState`, `OperationContext`,
  `service_failure::Code`, `infra_http::Problem`, `&mut Tx<'_>`). Keep a new
  crate, module, trait, wrapper, or helper only when deleting it scatters an
  invariant across callers or violates a dependency, compile-time, or lifecycle
  constraint.
- **Seam discipline & replace-don't-layer:** *one adapter is a hypothetical
  seam; two real adapters (or a true external seam) make a real seam.* Shape
  interfaces so observable behavior sits at the public seam without `pub`
  internal leaks, and list superseded shallow modules, wrappers, and internal
  tests to delete in the same change.

## Dependency Categories

1. **Pure / in-process** (domain logic, state machines, codecs,
   `crates/shared-rules`): no port or trait; absorb into a deep module. Pass
   `now` or generated IDs as plain parameters into pure logic rather than
   inventing a `Clock`/`Rng` trait.
2. **Local-substitutable / real workspace backend** (`infra-postgres`,
   `infra-cache`, `infra-messaging`, `infra-object-storage`, Axum `Router`):
   keep the seam internal or at the transport interface rather than adding a
   mock-only trait, so Implementation can verify against the real local store
   or router.
3. **Remote / true external seam** (`infra-outbound-http`,
   `infra-outbound-grpc`, OIDC, webhooks): expose the real client adapter
   rather than a mock-only trait so Implementation can verify at a local wire
   seam, adding a minimal port/trait only with ≥2 real adapters or pure
   transport-independent orchestration.

## Rust Contracts, Compile Reality, And Workspace Graph

- **Type-encoded invariants & error seams:** encode validated values, ordering,
  tx scope (`&mut Tx<'_>`), and resource ownership in Rust types and lifetimes
  under `rust-idiomatic`. Fix each seam's typed error boundary and its mapping
  to `service_failure::Code` and retryable vs terminal semantics.
- **Static compile-reality check:** inspect `Cargo.toml` `[workspace.lints]`,
  `clippy.toml`, `vendor/*/PATCHES.md`, and non-obvious `Cargo.lock` crate
  sources directly. Verify signatures and caller sketches against `Send + Sync
  + 'static` across `.await`, `dyn` object safety (prefer concrete types,
  closed enums, or generics), `&mut Tx<'_>` / `OperationContext` lifetimes,
  `FromRequestParts` vs `FromRequest` order, and `sqlx::query!` / `utoipa`
  bounds. Flag any statically unprovable type or macro assumption in
  `Assumptions` (`Reopen if`) + `Planning hooks: integration-first`.
- **Workspace ownership, config & profiles:** reconcile placement and edges
  against [`quality/architecture.json`](../../../../quality/architecture.json)
  (including `allow_dependents`) and [Repository
  Architecture](../../../repo-architecture.md#global-invariants): business
  behavior in `crates/<feature>`, transports/providers in `crates/infra-*`,
  config structs and startup validation in `crates/config`, and binary wiring
  in composition roots (`crates/service`, `crates/jobs-worker`—sharing feature
  logic across binaries without duplicating business rules; keep
  `crates/migrate` restricted to its `migrator` dependencies). Keep
  `crates/shared-rules` `std`-only, optional profiles independent and
  registered in `scripts/template/profiles.json`, and generated contracts
  (`api/openapi/service.yaml`, `crates/grpc-contracts`, `.sqlx/`, harness
  files) under one canonical writer.

Record results in [Design V1](../../interfaces/design-v1.md).
