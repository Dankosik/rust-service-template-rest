# Ownership Map V1

Status: ready. Scope: [system](system.md) and [inventory](inventory.md).
No new Rust crate or exported runtime interface. Existing crate boundaries,
generated contract authority, lifecycle/error owners and pinned dependencies
remain. The profile transformation removes existing owned behavior physically.

## Responsibilities

| Responsibility | Affected paths / exact action | Current evidence / semantic owner | Boundary and cleanup | Proof owner / reopen |
| --- | --- | --- | --- | --- |
| Initialization | Add init-module.sh, template_init.py, template_profiles.json; retain local after init | No existing initializer; specification owns one-shot behavior | Standard library policy over staged source; remove source-only fixtures and unselected packs; no runtime dependency | Source-only initializer fixtures + 16 outputs; reopen System for unsupported transformation shape |
| Snapshot and admission | Add template_state.py | Existing Git CLI and sync-cli parser; common owner of lock/path/blob/plan policy | Stable schema-v1 shared functions used by initializer and sync; no service code execution, no reset; preserve initializer-facing schema-v1 API when portable helper updates | Safety fixtures and canary; reopen System if another authority is needed |
| Portable adoption | Add template-sync.sh/template_sync.py, template-owned.paths, docs/template-sync.md | Existing narrow generators; manifest sole full-copy owner | Plan/compare/apply stage; owned removals only; no profile restoration | Sync fixture snapshots and canary; reopen Specification for preservation change |
| Cargo service identity | Change service Cargo manifest, root repository metadata, lock; explicit library name | Cargo is package/binary authority; current imports use service::api | Rename package/main binary, retain crate directory and library imports; no new crate | locked metadata/build/matrix; lock failure reopens Research |
| Runtime identity | Change observability default and API annotations, lifecycle executable test key | config owns snapshot; service API owns annotations | Generate YAML through existing binary; no separate YAML writer | existing contract/process tests + matrix; reopen System for runtime identity contract change |
| PostgreSQL projection | Mark/remove exact inventory files in config, service, provider/migrator/test | Existing config/bootstrap/shutdown/provider boundaries | No abstraction or new lifecycle owner; keep common cleanup, remove pool-specific tuple fields/params/branches together | retained-profile tests and DB gates, no-profile build/check; reopen System if physical removal changes lifecycle |
| Portable commands and local facts | Refactor Makefile/template.mk; add service.mk/profile-postgres.mk/source.mk; change listed generic CI helpers | Existing template.mk and optional service.mk split | Local data/recipes before method; DB recipes local; matrix source-only; no target Make eval during sync | classifier/verify self-tests + matrix; reopen System if a standard override is needed |
| Harness projection/preservation | Change existing agent-roles, codex-agents, harness-skills, sync-cli/check-skills only for selection and admission; lexical settings owner in template_sync.py | Existing canonical roles/classes/config and generated view layout | Preserve all six existing carriers' semantics; remove unselected generated paths; keep marked service skills and managed JSON exception | generation checks + sync negative fixtures; reopen System for a new adapter |
| Portable instructions/local policy | Refactor only inventory manifest docs/AGENTS/skills as needed to link local owners | Prompt Maintenance and current local architecture/commands | Move facts without new macro policy; preserve skill triggers, role authority and validation standards; no new skill/profile | check-instructions, links, contrasting static fixtures and neighbor review; reopen Definition for new reached discipline |
| Initializer CI and purity | Add source matrix/safety fixtures and source.mk; change classifier, verify, ci required gate | Existing changes/required chain and shared validation lock | One serial 16-output job, no source fixtures in outputs; all ordinary gates remain | actual matrix and canary at final validation; no remote CI claim from local runs |

Reuse rungs for nonmechanical additions: Git object plumbing and Python 3.11
stdlib are the selected installed mechanisms; existing Bash generators remain
projection owners. The strongest rejected external source is Copier 9.18.2
(broad merge/update semantics), with cargo-generate 0.25.0 for generation only.
Neither supplies the accepted manifest/dirty/settings policy. Parity is judged
by specification behavior and the composition canary, not output similarity to
Go. Reopen when a maintained mechanism eliminates those custom gaps. No
dependency/toolchain upgrade is part of this design.

## Files: materially changed Rust sources

| Path | Responsibility / present reason | Declarations / visibility / call path | Lifecycle/error owner; dependencies; exclusions |
| --- | --- | --- | --- |
| `crates/config/src/observability.rs` | Runtime identity + PostgreSQL projection | Existing Otel defaults and docs, no public shape change | Config validation stays; no provider dependency |
| `crates/config/src/lib.rs` | PostgreSQL projection | Existing postgres module/re-export, Config field/validate call removed only for none | Config remains sole typed owner; no transport/lifecycle code |
| `crates/config/src/load.rs` | PostgreSQL projection | Existing load test loses DB-only assertion | Loader precedence retained; no new settings/secret mechanism |
| `crates/config/src/secret_policy.rs` | PostgreSQL projection | Existing tests use DB vectors only when retained | Generic secret predicate unchanged; no weakened file secrecy |
| `crates/config/src/cli.rs` | PostgreSQL projection | Existing migrator test block removed only for none | Existing CLI errors unchanged; no second parser |
| `crates/config/src/postgres.rs` | PostgreSQL projection | Existing whole module removed for none | Existing validation/error types retained unchanged for postgres |
| `crates/service/src/api.rs` | Runtime identity | Existing private ApiDoc annotations; public document/render/contract unchanged | Generated OpenAPI owner; no YAML patch path |
| `crates/service/src/bootstrap/mod.rs` | PostgreSQL projection | Existing private open_postgres and DB BootstrapError variants/Prepared fields; DB blocks only | Composition root keeps signals, readiness, metrics and cancel ownership; no feature/provider migration |
| `crates/service/src/bootstrap/shutdown.rs` | PostgreSQL projection | Existing private dependency-close parts, parameter/tuple members and ShutdownPlan fields | Common joined shutdown and budgets retained; no parallel cleanup or alternate success semantics |
| `crates/service/tests/lifecycle.rs` | Runtime identity | Black-box executable lookup follows new bin key | Test retains process lifecycle proof; no unit choreography assertions |
| `crates/infra-postgres/src/{lib,dsn,pool,probe,transaction}.rs` | PostgreSQL projection | Existing files removed as pack for none | Provider semantics and visibility unchanged for postgres |
| `crates/migrate/src/{lib,main}.rs`, `crates/migrate/build.rs` | PostgreSQL projection | Existing embedded migration runner removed as pack for none | Migrator owns its current failure/commit semantics when retained |
| `test/src/lib.rs`, `test/tests/postgres.rs` | PostgreSQL projection | DB helper library and DB proof removed for none | Preserve independent utility recipe tests; no production dependency |

Brace rows designate those exact existing files, not a wildcard permission.
No changes to `crates/service/src/lib.rs`, `src/bin/openapi.rs`, or
`tests/openapi.rs` are needed for library renaming: explicit library name keeps
their imports valid. `test/Cargo.toml` retains package `integration-tests` even
without its DB-only lib.rs; integration tests compile independently. No
transport/health/telemetry implementation changes are selected.

Marker-enabling formatting is allowed only in the named mixed owners: expand
parameters and tuple components to complete lines so their existing DB parts
can be removed with balanced markers. Keep common result/cleanup ownership;
do not move code into a new runtime module to make templating convenient.
Docker similarly separates main and migrator cook/build/copy RUN statements
where whole-line markers require it, retaining auditable compilation and
identical final image semantics for PostgreSQL.

## Review routing and stage obligations

The changed profile spans several crates and generated/manual containment;
the Rust Ownership Review panel is therefore triggered. Its three nonoverlapping
lenses inspect responsibility/path ownership, crate/visibility/generated
containment, and file cohesion/test placement. Technical Design Review consumes
their receipts and reviews flows, admitted writes, preserved content, lock,
portability and gate coherence without repeating ownership lenses.

Stage-7: portability touches current methods only. Add contrasting static
fixtures for changed local-authority routing, profile availability and skill
preservation; review adjacent triggers. No stage-10 capability skill or Go
universal discipline is reached. Planning must record the existing first
dispatched-ledger obligation if it selects a ledger; these design/review lanes
are not that execution evidence.
