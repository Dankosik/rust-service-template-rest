# Rust Architecture And Ownership

Load from [Technical Design](../technical-design.md) when crate or module
placement is open—or, even with fixed files, when a choice changes who enforces
an invariant, what callers must guarantee, or which module may depend on
another—and current code,
[`quality/architecture.json`](../../../../quality/architecture.json), and
[Repository Architecture](../../../repo-architecture.md) do not mechanically
force the answer. Own semantic ownership and cross-boundary Rust contracts
without writing function bodies or guessing a brittle internal `.rs` file list.

Apply `rust-coder`'s earliest-owner rule and `rust-structural-quality`'s
deletion test to the ready spec/system design, current callers, the composition
root, generated contracts, and replacement paths.

## Deep Modules And Seam Discipline

- **Deep modules over shallow layers:** Design modules with a small,
  high-leverage **interface** (everything a caller must know: types, methods,
  invariants, ordering, error modes, lifetimes) hiding complex logic behind it
  so change, bugs, and verification stay localized (**locality**). Collapse
  shallow pass-through chains (`handler -> service -> repo -> mapper`) that
  relocate reasoning across files without owning a distinct invariant.
- **Deletion test & caller walkthrough:** For an open ownership choice, walk an
  affected caller through each viable option (what must the caller know or do,
  and where is the invariant enforced?) and trace which callers change together
  if the rule changes. Imagine deleting every proposed crate, module, trait,
  wrapper, or helper; retain it only when deletion scatters an invariant or
  complexity across callers, or violates a present dependency-direction,
  compile-time, or lifecycle boundary.
- **Seam discipline:** *One adapter is a hypothetical seam; two real adapters
  (or a true external boundary) make a real seam.* Do not introduce a
  single-implementation trait solely to mock internal logic or PostgreSQL.
- **The interface is the test surface:** Callers and tests cross the same seam.
  Shape interfaces so Implementation can verify observable behavior through the
  seam without exposing `pub` internals, while leaving concrete test cases,
  fixtures, and commands to Implementation.

## Dependency Categories

Classify each non-trivial dependency to decide where a seam belongs:

1. **Pure / in-process** (domain logic, state transitions, validation, codecs,
   `crates/shared-rules`): no port or trait; absorb directly into a deep module
   and test through its interface.
2. **Local-substitutable / real workspace backend** (`infra-postgres`,
   `infra-cache`, `infra-messaging`, `infra-object-storage`, in-process Axum
   `Router` via `tower::ServiceExt`): keep the seam internal or at the
   transport boundary; test through the real local store or router rather than
   inventing a mock-only repository trait.
3. **Remote / true external boundary** (`infra-outbound-http`,
   `infra-outbound-grpc`, external providers such as OIDC or webhooks): place
   an explicit seam—either the real client adapter verified against a local
   wire fixture (loopback HTTPS / `wiremock`), or a minimal port/trait when ≥2
   real adapters exist (or pure orchestration must be verified independently
   of transport).

For a hard-to-reverse cross-crate interface fork, use [Design It
Twice](design-it-twice.md).

## Rust Contracts, Compile Reality, And Crate Graph

- **Make invalid states unrepresentable:** Encode ordering, validated inputs,
  transaction scope, and ownership transfers in Rust types, enums, and
  lifetimes rather than caller discipline. Include a concise Rust
  type/trait/enum signature in `design.md` only when it pins a boundary
  invariant more precisely than prose.
- **Static compile-reality check:** Verify every proposed Rust seam against
  current code and `Cargo.lock` crate sources before finalizing: `Send + Sync +
  'static` bounds across `.await`, `dyn` object safety (preferring concrete
  types, closed enums, or generics over `dyn` per `rust-structural-quality`),
  `&mut Tx<'_>` and `OperationContext` lifetime propagation,
  `axum::extract::FromRequestParts` vs `FromRequest` ordering, and
  `sqlx::query!` / `utoipa` constraints.
- **Crate graph & optional profile isolation:** Reconcile every crate edge
  against [`quality/architecture.json`](../../../../quality/architecture.json)
  and [Repository Architecture](../../../repo-architecture.md#global-invariants):
  feature crates stay free of unapproved transport and provider dependencies;
  `crates/shared-rules` stays pure `std`-only; optional template profiles stay
  independent and inert unless selected; generated contracts
  (`api/openapi/service.yaml`, `crates/grpc-contracts`, `.sqlx/`, materialized
  harness files) keep one canonical writer.
- **Reuse & cleanup:** Verify non-mechanical reuse rungs (workspace -> `std` ->
  locked crate -> new crate) against `Cargo.lock`, [Backend Library
  Selection](../../../backend-library-selection.md), and `docs/decisions/`.
  Explicitly name superseded code, config, contracts, or tests to delete in the
  same change.

## Handoff And Refinement

Record the resulting `Semantic owners`, `Deep modules & seams`, `Verified
anchors`, `Shared mutated surfaces`, and `Planning hooks` in [Design
V1](../../interfaces/design-v1.md) so Planning can slice vertical tasks and
assign disjoint `Mutable owners` and `Exclusive locks` without guessing private
file trees.

When Implementation later uncovers evidence that improves local placement,
update affected entries in `design.md` and recheck only invalidated decisions
and proof while preserving unaffected boundaries.
