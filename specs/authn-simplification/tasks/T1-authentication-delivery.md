# T1 — Contract-derived, library-backed authentication

Outcome:
Replace per-handler authentication and the redundant provider parsing/transport paths with the reviewed final-router policy and bounded library-backed engines, so the complete served API enforces its declared policy independently of idempotency, with sealed verified identities and compatible initializer outputs.

Consumes:
- [Intent](../intent.md) and [Specification](../spec.md#outcome-and-boundary) — authorized behavior, exclusions and R1–R10 obligations.
- [System design](../design/system.md#route-construction-and-final-enforcement) — actual-method registration, final policy and auth-before-idempotency.
- [Configuration and shared boundary types](../design/system.md#configuration-and-shared-boundary-types), [JWT](../design/system.md#jwt-preparation-key-eligibility-and-verification), [introspection/transport](../design/system.md#introspection-and-trusted-transport), [refresh](../design/system.md#refresh-state-and-lifetime) — closed mechanisms and exported evidence.
- [Exported contracts](../design/ownership.md#exported-contracts-and-independent-ownership), [responsibilities](../design/ownership.md#responsibilities) and [file ownership](../design/ownership.md#files) — allowed dependencies, exact placement and deterministic existing-caller migration.
- [Technical Design transition](../technical-design-transition.md) and [PASS](../technical-design-review.md) — ready inputs, crypto material-admission repair and unchanged semantic review scope.

Provides:
- One assembled candidate implementing R1–R10, with executors' claim-matched test changes and final-validation command notes, generated OpenAPI/profile closure, and removal of superseded paths.
- An Implemented handoff identifying candidate and changed surfaces after all writers join; final verification and review remain pending until the assembled boundary.

Boundary:
Implement the accepted Stage 10.1 repair and its direct consumers/projections. Keep one canonical route document/provenance owner, one adapter URL grammar, library signature/registered-claim validation and existing process-owned refresh lifetime. Retain jsonwebtoken/reqwest and the selected pinned backend; add only the reviewed JWT-only direct aws-lc-rs edge, without an upgrade. Config stays independent of provider/HTTP types. Bootstrap remains the sole conversion/preparation/lifecycle owner. Source compatibility changes in the template are deliberate; do not add a compatibility shim, new service, cache, role framework, health probe or validation environment. Stage 10.2 outbound policy, persistence semantics, merge, deployment and live-provider certification are outside this unit.

## Accepted deltas

All implementation-changing obligations belong to T1; the links retain the detailed semantics rather than create a second specification.

| Accepted obligation | Current-to-target action and responsible ownership-map IDs |
| --- | --- |
| [R1](../spec.md#r1-one-route-contract) | Replace opt-in protect/opaque served split with actual-method carrier, final document policy and outer authentication; keep native fallbacks/HEAD and provider-free probes; independent verifier in bootstrap. R/H/D/B. |
| [R2](../spec.md#r2-jwks-interoperability-and-key-selection) | Replace whole-set rejection with per-entry admission, supported material import, fixed algorithm bindings/conflict rejection and unambiguous selection; preserve last usable snapshot. J/F. |
| [R3](../spec.md#r3-operator-diagnostics-safe-caller-errors) | Add safe contextual preparation failures and bounded closed runtime/refresh reasons while caller Problems remain fixed. V/U/J/I/F/B/H. |
| [R4](../spec.md#r4-jwt-trust-and-typed-claims) | Replace generic claim visitors/signature-only validation with typed verified decode plus library Validation and accepted profile/identity checks; retain the reviewed nbf:null distinction. V/J. |
| [R5](../spec.md#r5-trusted-authentication-transport) | Use canonical ProviderUrl and pooled trusted HTTPS client with existing byte/time bounds; delete auth DNS path/dependency/tests only. U/B/P. Independent outbound DNS is preserved. |
| [R6](../spec.md#r6-verified-scopes-and-expiry) | Expose sealed verified scope/expiry, coherent normalization and the small explicit scope-denial helper. V/H. |
| [R7](../spec.md#r7-introspection-classification-and-capacity) | Add typed active classification, configured immediate capacity and explicit request Timeout; remove forced pending/sleep and fixed capacity authority. I/V/C/H. No cache/retry. |
| [R8](../spec.md#r8-shared-refresh-lifecycle) | Publish immutable key snapshots, independent process-owned fetch budget and generation completion with request-local waits; preserve periodic/cooldown/singleflight and joined shutdown. F/J/B. |
| [R9](../spec.md#r9-configuration-and-crypto) | Replace dormant cross-mode fields/duplicate URL parser with tagged inputs, exact audience normalization and allowed algorithm/profile/concurrency conversion; retain crypto feature discipline. C/U/J/B/P. |
| [R10](../spec.md#r10-bearer-envelope-projections-and-development-policy) | Enforce bounded envelope before copying, preserve fixed status/challenge semantics, migrate whole-file engine projections/dependency closure and canonical authoring/development guidance. V/H/P. |

Optional recommendations already dispositioned upstream remain closed: include require_scope; do not add introspection caching or replace the retained JWT/cache architecture with an all-in-one crate. Record the trusted-auth versus untrusted-outbound distinction without changing Stage 10.2. No accepted recommendation is deferred to an untracked follow-up.

Mutable owners:
- Config input/loader and tests; adapter provider, claims, bearer, JWT, introspection, refresh and public surface; infra-http contract/auth/public router/idempotency composition; service API/bootstrap and jobs public-router consumer, with owner-local tests and existing mounted consumers. Exact paths and mechanical-caller rule are in [Files](../design/ownership.md#files).
- Workspace/affected manifests and intentional lockfile delta; canonical initializer definitions/scripts/projection checks; generated OpenAPI; listed authentication/architecture/config/authoring/roadmap and matching template documentation. Instruction carriers only when removing stale auth guidance, under their existing maintenance owners.

Exclusive locks:
- Adapter public `lib.rs` and its shared claims/options/error contracts: one writer at a time; internal engine lanes consume the agreed contract.
- Infra-http contract/public exports and composition surfaces: one writer for shared `lib.rs`; service/bootstrap/API/jobs/idempotency integration has one assigned composition owner, with overlapping HTTP ownership serialized.
- Workspace/affected dependency manifests and Cargo.lock: one integration owner; projection writers coordinate any markers in source files with their current writer.
- Canonical template profile/generator and generated OpenAPI output: one writer each, after the authoritative API/source is available. Never edit generated OpenAPI as source.
- Shared repository validation lock: only the delivery owner uses it after all code is assembled; no concurrent CPU-heavy validation.

Config, adapter internals, HTTP/composition and docs/template preparation may run as subtask lanes from the accepted semantic contracts. The Lead assigns precise disjoint files before dispatch; no two lanes write a shared lib.rs, bootstrap, manifest or marker-bearing file. Export spelling can be settled by the responsible writer without reopening accepted semantics. Projection finalization consumes the actual exported names and dependency closure. These are coordination constraints, not fixed waves or additional ledger tasks.

Final validation:
- Claim: The assembled candidate meets R1–R10 through its real composition path, including no-idempotency/public/no-auth variants, and removes obsolete owners without altering Stage 10.2 semantics. Scope and required material coverage remain exactly [Specification's proof boundary](../spec.md#proof-and-completion-boundary).
- Checks: Ordinary matching build and relevant passing tests under [Validation budget](../../../AGENTS.md#validation-budget), with the mixed-surface route selected by the delivery owner and reused scoped evidence where valid. Executors select cases, fixtures/assertions and commands while coding; preserve existing applicable gates and use existing TLS/mounted harnesses. Documentation/projection/instruction checks apply to changed carriers. No new exhaustive matrix, database environment, live IdP or performance target is added. One fresh independent assembled authorization/lifecycle delivery review is required by the accepted Specification. Concrete commands belong in executor handoff notes, not a prior test-design gate.
- Observable: Required tests actually exercise the changed behavior and affected entry points build; canonical/generated artifacts agree; no known in-scope defect or unresolved blocking review finding remains. Expensive runtime proof is factored from structural profile/harness projection checks. This establishes local acceptance only. Root then commits/pushes the accepted candidate, opens the separate PR and obtains applicable exact-head CI evidence; those external outcomes are tracked in [Completion](../tasks.md#completion-state), with no merge/deploy.

Reopen if:
Caller behavior must differ: Specification. A actual-method bypass, unsupported pinned API/material-import mechanism, changed trust/destination/lifecycle boundary or inability to retain normative validation: System Design (and Research only for disputed library evidence). A newly necessary semantic file/module/dependency owner outside the map: Rust Ownership. More than one independently consumable outcome or materially different external gate becomes necessary: Planning. Routine helper bodies, exported spelling, existing-caller migration, concrete tests and command selection stay executor-owned. Missing optional environments limit evidence without blocking supported implementation.
