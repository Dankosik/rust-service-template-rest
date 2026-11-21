# Rust Architecture And Ownership

Disclosed branch of [Technical Design](../technical-design.md) (audited in
[Design Review](../design-review.md)). Own crate/module ownership, deep Rust
seams, and compile reality under `rust-idiomatic` and `rust-structural-quality`
(loading `rust-axum`, `rust-api-contract`, `rust-errors`, or `rust-dependencies`
when their trigger fires).

## Vocabulary And Deep-Module Discipline

Keep terminology exact across `design.md`: use **seam** for a code interface
boundary (distinct from a runtime **crossing**), treat **interface** as
everything a caller must know beyond the type signature (invariants, ordering,
error modes, lifetimes, cost), and judge **depth** by caller **leverage** and
maintainer **locality** rather than line count.

Apply four seam rules:

1. **Highest existing seam first:** extend existing seams before adding new
   ones; place any new seam at the highest observable point that hides wiring
   (the fewer seams, the better). **One adapter means a hypothetical seam; two
   real adapters mean a real seam.**
2. **Three deepening questions on every proposed seam:**
   - Can I reduce the number of methods or collapse multi-step caller
     coordination into one deep call?
   - Can I simplify the parameters or encode invariants and tx scope
     (`&mut Tx<'_>`) directly in types?
   - Applying the **deletion test**: if this module, wrapper, or trait
     vanishes, does complexity vanish (shallow pass-through) or reappear across
     callers (earned depth)?
3. **Caller walkthrough (happy + worst-case failure/cancel):** for every
   non-trivial new or changed seam (`S<n>`), write a realistic 3–8 line
   call-site sketch against live workspace types (`AppState`,
   `OperationContext`, `service_failure::Code`, `infra_http::Problem`,
   `&mut Tx<'_>`) covering **both** the happy path and the hardest failure,
   timeout, or cancellation path. If the caller must coordinate magic call
   order, hold `&mut Tx<'_>` across network `.await`, or match internal storage
   errors, deepen the seam.
4. **Replace, don't layer:** keep the public interface as the proving surface
   (internal seams stay private behind the module) and list superseded shallow
   modules, wrappers, and internal-only tests to delete.

## Dependency Categories

Classify every dependency behind a seam to decide whether a trait is earned:

1. **Pure / in-process** (domain logic, state machines, codecs,
   `crates/shared-rules`): concrete types and functions; pass `now` or
   generated IDs as plain parameters instead of `Clock`/`Rng` traits.
2. **Local-substitutable / real workspace backend** (`infra-postgres`,
   `infra-cache`, `infra-messaging`, `infra-object-storage`, Axum `Router`):
   keep the seam internal or at the transport interface with concrete adapters
   so Implementation verifies against the real local store or router (no
   fake-adapter port trait wrapping a local backend).
3. **Remote / true external seam** (`infra-outbound-http`,
   `infra-outbound-grpc`, OIDC, webhooks): keep the real client adapter at the
   wire seam; introduce a minimal port/trait only with `≥2` real adapters or
   pure transport-independent orchestration.

## Simplicity-First Compile Reality And Workspace Graph

- **Kill tricky bounds before proving them:** default to concrete structs with
  inherent `async fn`, `axum::middleware::from_fn_with_state`, and ordinary
  enums over `async` traits (RPITIT `!Send` across `tokio::spawn`), custom
  `tower::Layer`/`Service`, generic `utoipa::ToSchema` wrappers, or typestates
  unless an accepted constraint genuinely demands them.
- **Honest verification bar:** check [workspace utility
  crates](../../../../.agents/skills/rust-dependencies/references/workspace-crates.md),
  `Cargo.toml` `[workspace.lints]`, `clippy.toml`, `vendor/*/PATCHES.md`, and
  locked crate APIs (`cargo metadata --locked --offline --format-version 1`).
  Treat tricky bounds (`async fn` trait `Send`, `&mut Tx<'_>` or guards across
  `.await`, `FromRequest` body consumption vs `FromRequestParts`, `dyn` object
  safety, `utoipa::ToSchema`, `sqlx::query!` custom types) as verified only
  after inspecting their defining trait or macro source. Whenever trait-solver
  or macro resolution remains uncertain statically, classify it honestly as an
  **unverified compile assumption** (`A<n>`) routed to `[integration-first]`
  instead of claiming static proof.
- **Typed error seams & exhaustive workspace types:** pin each seam's typed
  error boundary, `service_failure::Code` mapping, and retryability. Public
  workspace enums and structs are exhaustive (`rust-idiomatic`): inspect
  cross-crate fan-out before adding a variant or field to wide exhaustive types
  (`service_failure::Code`, `AppState`, or public `service_config` enums)—prefer
  mapping feature-local errors onto existing `Code` variants, or record the
  blast radius in `Handoff deltas for Planning`.
- **Workspace graph & profiles:** reconcile crate placement,
  `roles.<role>.allow_roles`, `allow_members`, `allow_dependents`,
  `crates/shared-rules` (`std`-only), optional profiles
  (`template/profiles.json`), and generated-contract owners against
  [`quality/architecture.json`](../../../../quality/architecture.json) and
  [Repository Architecture](../../../repo-architecture.md#global-invariants)
  (sharing feature logic across `crates/service` and `crates/jobs-worker` when
  both binaries are retained).

*Done when:* every new or changed `S<n>` has a happy + worst-case failure/cancel
caller walkthrough against verified workspace types, a justified dependency
category, reconciled `quality/architecture.json` and exhaustive-type blast
radius, and zero unacknowledged compile assumptions.
