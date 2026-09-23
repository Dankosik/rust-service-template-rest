# Bounded outbound HTTP: Ownership Map V1

Status: ready; ownership panel and Technical Design Review PASS. Behavior: [Specification](../spec.md). Mechanisms and public
signatures: [system design](system.md). Baseline source is
`098b4ab18dd5b2d158a94e126798d8cc429ad735`. Existing source locators below name
current owners; new paths are proposed files, not links to missing artifacts.

## Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner and exact action | Dependency/composition/generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| Egress DNS | Existing `crates/infra-bearerauthn/src/provider/dns.rs` owns post-DNS predicate, Hickory RuntimeProvider/Spawn and cancellation scope | New `infra-egress-dns`: move generic DNS resolver/predicate/tracked runtime, expose neutral typed errors; keep lifecycle ordering and apply the explicit address-policy correction in system design | Leaf depends on reqwest DNS types, Hickory, Tokio, tokio-util; both transports depend on it; no auth/outbound reverse edge | Remove moved definitions from auth; retain auth test-only raw-answer facade and auth error translation | Leaf inline unit tests retain predicate and tracked-task proof; auth provider tests retain integration parity | Failed parity, required consumer-specific lifetime or a changed registry policy |
| Bounded exchange | Existing auth HTTP policy is deliberately private; reqwest 0.13.5 exposes required builder/DNS methods | New `infra-outbound-http`: public API, fixed target, limits, permit, absolute deadline, bounded response and static errors | Depends on leaf, reqwest, url, Tokio, tokio-util; provider adapter calls it; no feature/config/bootstrap import | No old general transport exists; do not add compatibility client | Inline tests beside transport and policy; local TLS test fixture only under cfg(test) | API cannot enforce accepted target/limit/lifetime without new policy |
| Inbound deadline observation | `crates/infra-http/src/harden.rs` stamps RequestDeadline; authn.rs reads it | Keep ownership in harden; public readonly type/accessor and lib re-export; select existing stamp under auth OR outbound | Consumer observes existing timer instant, never resets it; no dependency on outbound crate | Replace auth-only ownership of the three stamp marker regions; no duplicate timer | harden inline middleware tests, existing auth tests | Stamp order changes or a caller needs to extend the parent budget |
| Profile selection and compatibility | `scripts/lib/template_init.py`, `template_state.py`, `template_profiles.json` own canonical projection, strict lock, preflight | Add binary outbound choice, explicit lock field, shared marker selections and exact inventory variants | Source manifest/projection own retained Rust and doc inputs; Cargo.lock remains generated dependency authority | Default removes outbound pack; no profile migration; source-only fixtures still removed | Existing init safety/purity/sync/projection suites | New profile value, legacy shape or projection needs a public-preflight shortcut |
| Profile proof factorization | `scripts/tests/template-profile-projections.py` owns 48 canonical projections and non-harness equality; `scripts/ci/template-init-check.sh` owns six real initializations/build/test graphs | Add outbound dimension to keys, normalized locks, diagnostics and 12 graph loop; retain single candidate and sequential build lock | Projector remains internal; public initializer always uses full staged metadata/format/OpenAPI route | Update stale counts and candidate allowlist, not a second runner | Existing projection checker self-test, source suites and full initializer runner | Equality invalidated by harness-dependent runtime inputs |
| Adoption guidance | Architecture, first-feature, command, initializer, structure and roadmap docs are service/source-local owners | Add selected outbound guide and scoped markers; show explicit provider-owned limits/deadline/reserve/cancel integration | No portable manifest entry for runtime or profile-marked docs; no generated OpenAPI source change | Remove selected guide and all references when none; mark 10.2 complete only after implementation acceptance | docs-check and selection/link proof | A real provider introduces config/readiness/credentials |

For non-mechanical sources: Egress DNS uses the repository reuse rung with
current auth DNS as authority; strongest rejected source is Hickory's standard
Tokio runtime because per-lookup custody is missing. Parity is the existing
predicate/tracked-runtime/auth proof. Upgrade/replacement requires equivalent
cancel/join and destination admission. Bounded exchange uses maintained
reqwest/Hickory extension points with the resolved versions in research;
strongest rejected transport is raw hyper because it duplicates TLS/HTTP
assembly and violates the requested reqwest choice. The custom portion is only
current application policy absent from reqwest: authority/header/body limits,
propagation removal and permit/deadline ownership. Replace it if reqwest offers
an equivalent enforced API; no general framework is introduced. Inbound
observation and projection extend their existing owners, not a new abstraction.

## Rust files

| Path | Responsibilities | Present reason | Declarations/visibility | Call-path role | Lifecycle/error owner | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `crates/infra-egress-dns/src/lib.rs` | Egress DNS | Common neutral facade for two real transports | public `PublicResolver`, `ResolveError`, `admit_address`; private modules | Consumer construction and reqwest Resolve integration | Resolver error classification and borrowed tracker/token custody | reqwest DNS, Hickory, Tokio, tokio-util, std | HTTP policy, Failure, provider parsing, bootstrap |
| `crates/infra-egress-dns/src/address.rs` | Egress DNS | Pure current public-address predicate with its registry exceptions | crate-visible predicate; inline existing tests | Literal and returned-answer admission | No tasks; boolean/static denial | std::net | DNS lookup, credentials, HTTP |
| `crates/infra-egress-dns/src/runtime.rs` | Egress DNS | One cancellation-aware Hickory runtime owner | crate-visible TrackedRuntime; private handle/drop guard; inline moved tests | DNS subtask creation, TCP connect/UDP bind | Per-lookup child cancellation and shared tracker retention | Hickory runtime traits, Tokio, tokio-util, std | New runtime, detached task, HTTP |
| `crates/infra-outbound-http/src/lib.rs` | Bounded exchange | Public facade, client state and execution owner | public Client/Limits/Request/Operation/Response/Error; private policy/test modules | Provider -> complete bounded exchange -> Response/Error | Own permit and send/body future; join delegated to caller tracker | reqwest, url, Tokio, tokio-util, infra-egress-dns, std | Raw client/builder export, retry, credentials/config/readiness |
| `crates/infra-outbound-http/src/policy.rs` | Bounded exchange | Cohesive target/header admission and limits validation | crate-visible helpers only | Pre-send target/headers and parsed response headers | Synchronous static errors | reqwest header types, url, std, facade value types | Network, parsing provider data |
| `crates/infra-outbound-http/src/tests.rs` | Bounded exchange | Black-box transport scenarios require local TLS fixtures while keeping production code closed | private cfg(test) module and fixtures | Drives public API plus internal test-only resolver injection | Tests join servers, close/wait trackers, use bounded waits | production types, Tokio and tokio-rustls dev dependency | Production private-address escape, public test-support feature |
| `crates/infra-bearerauthn/src/provider.rs` | Egress DNS | Preserve auth transport while consuming shared resolver | Existing visibility/signatures; changed imports and error translation only | JWT/introspection -> existing auth HTTP exchange | Auth keeps Failure, timeout/reserve/JSON/status rules | Existing deps plus infra-egress-dns | General outbound API/policy changes |
| `crates/infra-bearerauthn/src/provider/dns.rs` | Egress DNS | Keep existing auth raw-answer fixture and private error adapter without duplicate DNS mechanism | Small auth-private adapter/imports and cfg(test) RawAnswerResolver | Auth production uses leaf; fixture sends raw answer set through same predicate | Map neutral errors to Failure::Unavailable | infra-egress-dns, reqwest DNS, std | Copied Hickory runtime or predicate |
| `crates/infra-http/src/harden.rs` | Inbound deadline observation | Make same timer usable with outbound-only profile | Public RequestDeadline with private field/constructor and public at(); existing stamping | Hardened request extension before existing timeout | harden remains sole deadline constructor | Existing Tokio/axum/tower deps only | Second timeout, outbound client dependency |
| `crates/infra-http/src/lib.rs` | Inbound deadline observation | Export readonly deadline from existing crate facade | Public RequestDeadline re-export under request-budget marker | Adapter/handler reads extension | No added state/error/task | Existing harden module | Constructors or budget extension |

Do not split files further for size alone. Executor may add inline tests in
these owners; a fixture helper stays in tests.rs. The TLS fixture DER files are
copied from the existing auth test fixture material into
`crates/infra-outbound-http/tests/fixtures/` so outbound-only pruning cannot
remove their input. These are non-secret fixture bytes with the existing
fixture hostname; no production certificate/root API is added.

## Manifests and profile inventory

New `crates/infra-egress-dns/Cargo.toml` and
`crates/infra-outbound-http/Cargo.toml` inherit package/lints, default features
remain empty. Leaf enables only existing reqwest DNS types, Hickory
`tokio,system-config`, Tokio `net,rt,sync,time,macros`, tokio-util `rt`.
Outbound enables reqwest `rustls`, url `std`, Tokio `sync,time,macros`,
tokio-util `rt`; its dev Tokio and tokio-rustls features match the auth fixture
requirements. Auth replaces direct Hickory dependency with the leaf while
retaining its HTTP/TLS feature set. Root Cargo.toml declares both path deps,
with profile markers below. Cargo regenerates Cargo.lock deliberately for
the two new local packages; no hand-edited lock or version upgrade.

Use three derived marker names in the existing inventory schema:

| Marker profile | Selected when | Whole paths removed otherwise | Root/shared marked regions |
| --- | --- | --- | --- |
| `outbound-http` | OUTBOUND_HTTP=bounded | `crates/infra-outbound-http/`, `docs/outbound-http.md` | Root outbound path dependency; outbound guide links/owner rows/graph edges/examples |
| `egress-dns` | AUTHN != none OR OUTBOUND_HTTP=bounded | `crates/infra-egress-dns/` | Root egress path dependency; Hickory and tokio-rustls declarations (replace their old auth-only markers); shared owner/graph documentation |
| `request-budget` | AUTHN != none OR OUTBOUND_HTTP=bounded | none | harden's existing deadline type, mapper state and mapper regions; lib re-export; readonly deadline docs |

These are internal derived selections, never extra CLI or lock values. Keep
marker regions nonnested and source-inventory exact. The manifest enumerates
every path/id; inventory/source mismatch still refuses. Source workspace
carries all implementation packs for validation, as today. Public initializer
default `none/none` removes both new crates and all shared regions; no lock in
the uninitialized source continues to mean all source capabilities are present.

| AUTHN | OUTBOUND_HTTP | Auth crate | Outbound crate | Egress DNS / request budget |
| --- | --- | --- | --- | --- |
| none | none | removed | removed | removed |
| selected engine | none | retained | removed | retained |
| none | bounded | removed | retained | retained |
| selected engine | bounded | retained | retained | retained |

`template_state.py` owns OUTBOUND_HTTP_CHOICES, normalized `outbound_http`,
explicit-field detection and `profile --field outbound_http`. New schema-1
locks always record all four choices. Admit exactly the two historical key
sets (database+harness; database+authn+harness) and the new four-field set;
missing outbound means none only for those historical sets. Preserve legacy
lock bytes during matching replay. Unknown/partial shapes remain refusal.
The uninitialized-source profile selector returns bounded when the source
crate exists, analogous to auth source handling; it is not a generated-service
default. No arbitrary subset defaults are allowed.

`template_init.py` extends InitInputs, argument/env SingleValue admission and
profiles(), selected marker sets, postconditions and replay checks.
`--outbound-http` / OUTBOUND_HTTP is none by default. `_profile_data` admits
three exact inventory generations: pre-auth for its existing historical lock,
current auth-only inventory for a lock without outbound, and the new inventory
with all derived sections. Require explicit absence of the corresponding lock
field before accepting an older inventory; legacy replay cannot manufacture
new packs. Update optional lock edges for `ipnet` and `once_cell`: remove their
Hickory-induced features only when both auth and outbound are absent, retaining
the exact source/version/edge guards. Existing JWT crypto feature rules stay
owned by AUTHN. Full locked offline metadata remains the graph oracle.

`make/template.mk` passes OUTBOUND_HTTP through the existing template-init
environment path; `scripts/init-module.sh` remains the thin public entry.
If it already forwards all arguments without a closed choice list, no shell
edit is needed there. `template_sync.py` consumes normalized target profile
without adding runtime/docs to portable ownership. `template-owned.paths`
remains free of all profile-marked artifacts. Historical normalization cannot
re-enable a pruned pack. Do not add a new sync flag or migration mechanism.

## Exact documentation and validation owners

The new guide `docs/outbound-http.md` owns API use, finite explicit limits,
relative URL rules, response reserve/deadline/cancellation hookup, errors,
encoded-body/header limits, public-only DNS and absence of trace propagation.
Update selected regions in `docs/architecture/boundaries.md`,
`docs/architecture/integration.md`, `docs/architecture/runtime-lifecycle.md`,
`docs/architecture/http.md`, `docs/project-structure-and-module-organization.md`,
`docs/first-production-feature.md`, and `docs/build-test-and-development-commands.md`.
Update initializer values/lock/default/counts in `docs/template-sync.md` and
readiness of stage 10.2 in `docs/roadmap.md` only at completion. This stage has
no concrete provider config, OpenAPI, Docker, deployment, or CI gate change.
`docs/authentication.md` changes only if shared DNS location needs a link;
its existing policy text remains valid.

Update `scripts/tests/template-profile-projections.py`: DATABASE(2) × AUTHN(3)
× OUTBOUND_HTTP(2) × harness(8) = 96. Runtime identity contains all first three
choices; normalized tree equality erases only harness-specific carriers/lock
harness value and admitted identity differences, never outbound choice or
runtime files. Add changed-dimension self-test faults to prevent false reuse.
The canonical projector remains `_project_staged` and cannot be invoked as a
public unsafe shortcut. A failed projection prevents graph proofs.

Update `scripts/ci/template-init-check.sh` to 12 sequential core representatives
from one fixed private source candidate. Every representative invokes public
init (full metadata + formatting + OpenAPI preflight), then matching build
and test once. Scrub OUTBOUND_HTTP with the other ambient identity variables;
include it in names, graph keys and receipts. Preserve shared validation lock,
explicit caller cache, fail-fast receipt semantics and focused modes.
Extend the existing snapshot allowlist in
`scripts/tests/template-candidate-paths.txt` and its directory admission to
both new crates and changed selected guide, so uncommitted local delivery
is actually included. Do not widen the allowlist to arbitrary dirty files.

Extend existing `scripts/tests/template-init-safety.py` and
`scripts/tests/template-sync-canary.py` fixtures for unknown/default/selected,
lock shape, replay refusal, preflight-before-write, shared-pack retention,
changed-dimension equality and no sync resurrection. Update existing calls
constructing InitInputs, not an alternate test initializer. Keep source-only
runners and specs removed from generated outputs. `template-owned-purity.py`
continues to enforce portable ownership; adjust expectations only where the
new canonical inventory shape requires it.

Implementation's final-validation owner runs the matching source build/test,
docs-check, scoped shellcheck if the shell runner changes, dependency/security
checks required by CONTRIBUTING for manifest/lock inputs, and the accepted
initializer source/projection/12-runtime route. Tests and exact cases remain
executor choices, not a separate Test Design phase. Use existing runners and
shared lock; no CPU-heavy commands in parallel. This design creates no new
full-repository aggregate, database runtime, image or production proof claim.

Reopen ownership only when a new responsibility cannot fit this map. A changed
selected runtime input invalidates its runtime graph receipt; changing only a
harness carrier invalidates its projection/equality evidence without multiplying
Rust compilation across all harnesses.
