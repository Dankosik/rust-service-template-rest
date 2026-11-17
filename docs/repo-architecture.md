# Repository Architecture

This is the architecture front door for the service identified by
`crates/service/Cargo.toml`. It records only invariants that `Cargo.toml`,
[`quality/architecture.json`](../quality/architecture.json), and Rust types
cannot state by themselves; read code, committed schemas, tests, and `make help`
for concrete modules, types, and commands.

## Global Invariants

1. Business behavior lives in `crates/<feature>`; transports and providers in
   `crates/infra-<provider>`; concrete wiring and process lifecycle in the
   composition root `crates/service`. `quality/architecture.json` (`make
   architecture-check`) enforces the crate dependency graph: feature HTTP and
   gRPC modules may use `infra-http`'s and `infra-grpc`'s inbound contract
   surfaces, no feature depends on an outbound or persistence provider crate,
   and no crate a feature depends on may depend on that feature.
2. One fact has one canonical writer; generated or projected views (`api/openapi/service.yaml`
   from `utoipa` annotations, `crates/grpc-contracts` from `api/proto/`, `.sqlx/`
   from `migrations/*.sql` + `sqlx::query!`, and materialized harness files from
   `.agents/`) are derived and drift-checked, never hand-edited as authority.
3. Optional profiles remain inert unless selected and configured; a partial
   capability is not a supported architecture. A crate, module, or directory
   exists only with its first real artifact. `crates/shared-rules` stays pure and
   `std`-only under its crate-level membership test; `crates/json-payload`
   separately owns the `serde`-dependent finite-float payload check shared only
   by `infra-messaging` and `infra-jobs`; `crates/operation-context` owns the
   deadline and cancellation lineage its listed operation owners propagate.
   Production code that several crates need but that requires an external
   type narrows its inputs to `std` types and joins `crates/shared-rules`;
   when it cannot, each caller keeps its own copy until a single-purpose
   contract crate such as `json-payload` earns an owner.
4. Test code shared by several crates lives in a `crates/<topic>-test-support`
   crate with the `test` role in `quality/architecture.json`. It depends on no
   workspace crate, appears only in dev-dependencies, and leaves with the
   template profile of its last consumer. A helper that needs one crate's types
   stays behind that crate's `test-support` feature. `make architecture-check`
   rejects a `#[path]` that reaches outside its own package.
5. Trust, ingress/egress, dependency criticality, failure mapping (`crates/service-failure`),
   readiness (`crates/health`), and staged shutdown budgets (`crates/service/src/bootstrap`)
   are enforced explicitly at their owning runtime boundaries.
6. A task-local design may retain, replace, or remove current architecture,
   naming the changed owner and its proving surface.

## Domain Vocabulary

Keep only accepted cross-task domain terms whose interpretation changes
behavior, authority, or proof; task-local terms stay in their specification.

| Term | Means here | Does not mean | Semantic owner |
| --- | --- | --- | --- |

The template scaffold defines no domain terms.

## Policy Owners

- [Production Contract](production-contract.md) — service-owned capacity, consistency, trust, and recovery gates before production promotion.
- [Backend Library Selection](backend-library-selection.md) and `docs/decisions/` — benchmark evidence and rejected library/protocol alternatives.
- [Validation Routing](validation-routing.md) and [CI/CD Production Readiness](ci-cd-production-ready.md) — verification and delivery policies.
- [Railway Deployment Profile](railway-deployment-profile.md) and [Alerting](alerting.md) — deployment and SLO runbooks.
