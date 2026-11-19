# Rust Architecture And Ownership

Disclosed branch of [Technical Design](../technical-design.md). Own semantic
crate/module ownership and cross-seam Rust contracts under `rust-idiomatic` and
`rust-structural-quality` (loading `rust-axum`, `rust-api-contract`,
`rust-errors`, or `rust-dependencies` only when their trigger fires).

## Vocabulary And Deep-Module Discipline

Speak consistently in `design.md`: **module** (unit with interface and
implementation), **interface** (everything a caller must know—types, methods,
invariants, ordering, error modes, lifetimes, cost—broader than a signature),
**seam** (code location of an interface, distinct from a runtime **crossing**),
**adapter** (concrete implementation at a seam), **depth / leverage**
(capability hidden per unit of interface, never padded lines), and **locality**
(change, bugs, and verification in one place).

- **Depth belongs to the interface:** compose private helpers or internal seams
  without `pub` leaks, and collapse shallow pass-through chains (`handler ->
  service -> repo -> mapper`) enforcing no invariant.
- **Highest seam & caller walkthrough:** extend existing seams before adding
  new ones, placing new seams at the highest observable point hiding wiring.
  For each non-trivial new or changed seam, write a compact Rust signature and
  3–8 line caller sketch against real workspace types (`AppState`,
  `OperationContext`, `service_failure::Code`, `infra_http::Problem`, `&mut
  Tx<'_>`). Keep a new structure only when deleting it scatters an invariant or
  breaks a dependency, compile-time, or lifecycle constraint.
- **Seam discipline & replace-don't-layer:** use the dependency categories below
  to decide whether a trait is warranted. Preserve public proving surfaces
  without exposing internals solely for tests, and list
  superseded shallow modules, wrappers, and internal tests to delete.

## Dependency Categories

1. **Pure / in-process** (domain logic, state machines, codecs,
   `crates/shared-rules`): no trait; absorb into a deep module, passing `now`
   or generated IDs as plain parameters instead of a `Clock`/`Rng` trait.
2. **Local-substitutable / real workspace backend** (`infra-postgres`,
   `infra-cache`, `infra-messaging`, `infra-object-storage`, Axum `Router`):
   keep the seam internal or at the transport interface with no mock-only trait
   so Implementation verifies against the real local store or router.
3. **Remote / true external seam** (`infra-outbound-http`,
   `infra-outbound-grpc`, OIDC, webhooks): expose the real client adapter for
   local wire-seam verification, adding a minimal port/trait only with ≥2 real
   adapters or pure transport-independent orchestration.

## Rust Contracts, Compile Reality, And Workspace Graph

- **Type-encoded invariants & error seams:** encode validated values, ordering,
  tx scope (`&mut Tx<'_>`), and ownership in types and lifetimes; pin each
  seam's typed error boundary and mapping to `service_failure::Code` and
  retryability.
- **Exhaustive workspace types:** public workspace enums and structs are
  exhaustive (`rust-idiomatic`). Before adding a variant or field to a wide type
  (`service_failure::Code`, `AppConfig`, `AppState`), check cross-crate
  fan-out—prefer mapping feature-local errors onto existing `Code` variants, or
  surface the blast radius in `Shared mutated surfaces` and `Planning hooks`.
- **Compile-reality honesty:** check [workspace utility
  crates](../../../../.agents/skills/rust-dependencies/references/workspace-crates.md)
  before proposing custom codecs, retries, compact containers, or derive
  boilerplate, and verify against the lints, patches, and `Cargo.lock` sources
  grounded in step 1. Treat tricky bounds as verified **only** after reading
  their defining trait/macro source—`async fn` trait `!Send` (RPITIT across
  `tokio::spawn`), `&mut Tx<'_>` or guards across `.await` in Axum handlers,
  `dyn` object safety, `FromRequest` (last, body-consuming) vs
  `FromRequestParts`, `tower::Layer`/`Service`, `utoipa::ToSchema` on
  generic/newtype wrappers, and `sqlx::query!` custom types. Route any
  statically unproved bound or runtime quirk through [Technical
  Design](../technical-design.md#steps) step 2's uncertainty disposition.
- **Workspace graph & profiles:** reconcile crate placement, dependency edges,
  `allow_dependents`, `crates/shared-rules` (`std`-only), optional profiles
  (`template/profiles.json`), and canonical generated-contract writers
  against [`quality/architecture.json`](../../../../quality/architecture.json)
  and [Repository
  Architecture](../../../repo-architecture.md#global-invariants), sharing
  feature logic across `crates/service` and `crates/jobs-worker`.
