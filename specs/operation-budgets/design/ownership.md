# Operation budgets: ownership map

Status: ready. Candidate D1-r1 with [system mechanism](system.md).
This is Ownership Map V1. The responsibility rows own constraints and evidence;
the inverse map adds file-specific facts. Runtime implementation remains ahead.

## Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner and exact action | Dependency/composition/generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| C1 Neutral context | New `crates/operation-context`; existing gRPC `call.rs::Deadline` and jobs `Job` deadline/token | Add one leaf containing deadline arithmetic and context child/stop primitives; remove duplicate gRPC arithmetic by importing the leaf | Depends only on existing Tokio time and tokio-util cancellation; no runtime I/O, config, transport, provider, feature or `service-failure` dependency; no task spawning | Values drop normally; lifecycle owners retain guards/tasks | Leaf unit proof and architecture/profile gates | A neutral existing owner or different representation can satisfy exact same large-duration semantics with less machinery |
| C2 HTTP admission/context | `infra-http/src/harden.rs` fixes optional `RequestDeadline` beside relative Tower timeout | `infra-http` stamps one always-retained context and exact opening timer; add typed request/response extraction and cancellation custody; keep RequestDeadline as optional projection | Chain order and Problems remain transport-owned; context types use neutral leaf; no generated OpenAPI edit | Future abandonment cancels; successful headers transfer call guard to response body; EOF/error/drop releases it without new body timer | Mounted HTTP tests beside infra-http owner | Changed wire failure or placement changes request-origin/order |
| C3 gRPC opening/stream | `router.rs::deadline`, `call.rs` terminal body, `observe.rs` handoff | `infra-grpc` exposes opening and distinct ResponseContext, transfers cancellation/permit/upload lifetime into current terminal owner | tonic request extensions are delivery only; `Deadline` arithmetic moves C1; status projection remains gRPC | Existing autonomous response owner gains cancellation; no local-opening cancellation of valid later stream | Existing `infra-grpc/tests/transport.rs` plus current call tests | Missing/long caller no longer retains current stream semantics |
| C4 gRPC composed client | `client.rs::ClientTimeout`, `Client::call`, OAuth `grpc.rs::call` | `infra-grpc::client` adds opaque PreparedCall bound to the concrete client; fixes entry cutoffs before auth and owns send/body | Public opening context and request metadata access support OAuth; cutoffs/private client binding cannot be replaced or moved to another client; no new generic tower layer | Prepared future drop/upload/response use C3 terminal custody | gRPC client tests and OAuth gRPC fixture | Prepared abstraction needs provider/auth policy or permits local policy bypass |
| C5 Inbound auth wait | `infra-bearerauthn::{authenticate,lib,introspection,provider}`; JWT `refresh.rs` worker remains existing owner | Add context-aware verifier/authenticate path and request-bound provider cutoff for uncached introspection; cached Moka/JWKS callers bound their waiting only | Existing principal trust and failure catalog unchanged; provider owns request timeout; no second coalescing registry or background task | Dropped caller wait cannot cancel another caller token or process JWKS worker; Moka native takeover retained | Current auth fixtures and mounted HTTP/gRPC auth proof | Requirement changes to exactly-one surviving exchange or shared work outliving every caller |
| C6 Cache command | `infra-cache/src/lib.rs::run`, `connection.rs::command` | Fix context child at public entry before prep, pass same cutoff/cancel across connection wait and one command | No feature policy, new replay or supervisor cancellation | Request future cancellation releases its wait; Link recovery remains process-owned | Current cache tests/CI integration owner | Cancellation would alter native pending-response cleanup or recovery contract |
| C7 Outbound HTTP | `infra-outbound-http/src/lib.rs::{execute,exchange}`; reference #242 exact path diff | Same owner retains absolute D and post-prep/final checks, adds explicit context entry and honors supplied request context | Standard HTTP request/response stays public; parent gets clamped by current local limits; no OAuth policy | Dropped exchange ends local waiting; provider outcome may remain unknown | Existing outbound tests | Known provider outcome would be erased or dispatch can occur after cutoff |
| C8 OAuth composition | `infra-oauth2-client-credentials/src/{lib,grpc}.rs` | Keep credential/cache owner; HTTP uses same D/context, gRPC C4 starts before every token path; cancellation bounds waiting and no resource replay | OnBehalfOf, secret token storage and private-key trust stay unchanged; only resource client owns its interval policy | Existing credential lifecycle remains; caller cancellation does not cancel process/shared state | Existing OAuth HTTP tests and `crates/infra-oauth2-client-credentials/src/tests/grpc.rs` | Existing credential owner cannot retain acquisition provenance or interval selection |
| C9 S3 operation | `infra-object-storage/src/lib.rs` public operations and SDK config | Add context entries with one private path for finite admission/preparation/SDK/EOF D; supported SDK per-operation timeout override; retain read retries and mutation single attempt | No credential-provider refactor or new SDK version; no feature keys/content/retention policy | Existing operation guard+permit transfers only GET to C10; pending mutations keep OutcomeUnknown; definitive outcomes retain existing results | Current adapter stubs and routed S3 integration owner | Required native override unavailable or effect classification would change |
| C10 S3 resource lifetime | `infra-object-storage/src/download.rs`; current gRPC terminal-owner pattern | Local Download state cell owns SDK body, withheld chunk, permit, guard and timer handle; weak expiry/cancel task terminates even without polling | No generic common body adapter, transport-status dependency, producer or queue; public Download/Body API keeps metadata/integrity semantics | One terminal transition; resources/destructors outside lock; drop aborts timer and synchronously empties cell; timer failure observed | Download/adapter tests observe confirmed EOF and actual no-poll body+permit release | SDK Body polling cannot coexist with bounded short resource-cell lock |
| C11 Handler context | `infra-messaging/src/{consumer,registry}.rs` cancellation-only handler | Consumer creates context at existing handler origin, Registry carries it to typed handler; same deadline drives timeout/finality check | Domain event/wire schema unchanged; handler signature adaptation changes Rust API only | Existing handler child token cancelled on return, panic, timeout, drain; settlement owner unchanged | Registry/consumer and current JetStream CI tests | Context propagation needs changed settlement/retry semantics |
| C12 Job context | `infra-jobs/src/kind.rs::Job::{deadline,cancellation}` | Add `Job::context()` as child view of existing attempt authority | No engine/SQL/claim/lease change; old accessors retain existing source | Attempt supervisor remains owner; child cancel never cancels engine | Existing kind/attempt proof and carrier construction test | A new attempt origin or SQL behavior is required |
| C13 Portable custody and documentation | Workspace, architecture allow-list, initializer profile manifest/classifier, usage guides | Register leaf and exact allowed edges, preserve registry versions in deliberate lock update, update optional profile carriers and context/stream docs | Always-retained leaf has first real artifact C1; optional crates still prune as before; source-only specs never projected | Remove superseded timeout arithmetic and dead marker declarations only; preserve generated authority | Routed architecture/dependency/profile/doc checks at final validation | Profile removes a needed carrier or requires unrelated gate/runtime changes |

Non-mechanical reuse decisions: C1 uses the current repository Deadline plus
existing Tokio/tokio-util rather than introducing a framework. Its strongest
rejected source is `infra-http::RequestDeadline`, which is transport-coupled and
lacks lineage/large-duration support. Parity is existing gRPC remaining/wait
semantics and exact ordinary-cutoff behavior; upgrade only when those contracts
can be retained by a simpler maintained native API. C4 extends the existing
Client owner instead of an external resilience layer; its parity obligation is
both current interval policies, security and trace/status ownership. C10 adapts
current `call.rs` weak-resource ownership; poll-only DeadlineBody cannot meet
no-poll release. Its parity obligation is current length/checksum/final-chunk
behavior, plus finite resource release. No custom retry/general async framework
is selected.

## Files

`private` below means private or `pub(crate)` only where sibling modules need
the declaration. Common allowed dependencies are each crate's current manifest;
new leaf edges are listed in C13. Forbidden responsibilities refer to the owning
row and never authorize cross-owner policy. Tests extend existing owner files;
exact test cases remain Implementation work.

| Path | Responsibilities | Present reason and declarations/visibility | Call-path role | Lifecycle/error ownership | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- | --- |
| `crates/operation-context/src/lib.rs` (new) | C1 | Public Deadline/OperationContext/closed stopped reason, private arithmetic; inline tests | Neutral boundary value before all changed operations | No tasks/error projection; see C1 | Tokio time, tokio-util sync | Provider policy, retry, I/O, metrics, defaults |
| `crates/infra-http/src/context.rs` (new) | C2 | Public RequestContext/ResponseContext wrappers and extractors; private response-body cancellation wrapper and opening middleware | Hardened opening/extraction/body handoff | C2; sanitized missing-context rejection | C1, current HTTP/Body/Problem dependencies | Business handler/stream-duration policy |
| `crates/infra-http/src/harden.rs` | C2 | Existing public RequestDeadline projects C1; private harden wiring/error helpers | Replace relative timer at its current position | C2, current 504 response owner | C1 and current middleware deps | New global response cap or PG reserve |
| `crates/infra-http/src/lib.rs` | C2 | Export context wrappers; register context module; keep optional RequestDeadline export | Public transport entry | C2 | Existing module graph | Alternate context implementation |
| `crates/infra-http/src/authn.rs` | C5 | Existing private auth boundary passes opening context and checks before protected dispatch | Transport to Verifier | Existing sanitized auth mapping plus C2 expiry | C1, bearer | Provider clock/refresh ownership |
| `crates/infra-grpc/src/lib.rs` | C3, C4 | Export ResponseContext/PreparedCall at existing public surface | Tonic consumers and OAuth | C3/C4 | Leaf, current gRPC graph | Credential policy |
| `crates/infra-grpc/src/router.rs` | C3, C5 | Public ResponseContext wrapper (or re-export declaration from lib), private deadline/auth flow | Inbound admission then auth/business | C3/C5 | Leaf, current router deps | Response lifetime derived from opening cap |
| `crates/infra-grpc/src/call.rs` | C3 | Remove local Deadline in favor of leaf; private Lifetime/state gains cancellation custody | Native upload/response terminal owner | C3; keeps gRPC code/error projection | Leaf, existing mutex/futures/Tokio/body | Generic S3 body engine |
| `crates/infra-grpc/src/observe.rs` | C3 | Private response extension extraction transfers cancellation with lifetime | Existing observation-to-body handoff | C3; same terminal observer | Leaf through current call types | Fresh stream timer or duplicate metric owner |
| `crates/infra-grpc/src/client.rs` | C4 | Public opaque PreparedCall and prepare/send API; private interval selection | Ordinary and authenticated calls share same start/send path | C4; C3 body owner | Leaf and current tonic/tower/http | OAuth cache or retry |
| `crates/infra-bearerauthn/src/lib.rs` | C5 | Public context-aware verify beside standalone entry; one private enforcement path | Shared verifier edge | C5 | Leaf, current engine deps | New principal claims/trust rules |
| `crates/infra-bearerauthn/src/authenticate.rs` | C5 | Context-aware authenticated entry with existing observation guard | Transport envelope -> verify | Existing failure/cancelled counting | Leaf, existing bearer | Transport wire mapping |
| `crates/infra-bearerauthn/src/introspection.rs` | C5 | Private distinction between caller-owned no-cache call and Moka shared call | Provider invocation/cache retention | C5; Moka takeover stays native | Leaf and existing Moka/provider | WeakShared registry or token TTL redesign |
| `crates/infra-bearerauthn/src/provider.rs` | C5 | Private request-bound timeout path; existing independent provider path retained | reqwest dispatch/full JSON body | Existing ProviderFailure; parent wrapper checks | Leaf, current reqwest | Retry or shared-refresh cancellation |
| `crates/infra-cache/src/lib.rs` | C6 | Public context variants delegate existing convenience calls to private entry | Key prep -> one command | C6, Unavailable | Leaf, current cache deps | Feature fallback/TTL policy |
| `crates/infra-cache/src/connection.rs` | C6 | Private command receives same deadline/cancel or is enclosed by caller select; no recovery change | Acquire -> dispatch | C6 | Leaf, current Link deps | New reconnect owner/replay |
| `crates/infra-outbound-http/src/lib.rs` | C7 | Public context variant; current instant entry honors request context; private absolute exchange | HTTP full buffered operation | C7; current Error and Attempt | Leaf, current HTTP transport | Credential handling/retry |
| `crates/infra-oauth2-client-credentials/src/lib.rs` | C8 | Public authenticated context variant and private parent-aware acquisition/resource path | Token wait -> HTTP resource | C8; current AcquisitionError | Leaf, outbound HTTP, existing deps | New token source or refresh worker |
| `crates/infra-oauth2-client-credentials/src/grpc.rs` | C4, C8 | Existing public authenticated Client uses PreparedCall; remove duplicate timeout encoder/origin | Before credentials -> same send/body owner | C8; current acquisition status source | C1/C4 and existing optional gRPC deps | Resource timeout policy selection |
| `crates/infra-object-storage/src/lib.rs` | C9 | Public context variants for operations; private budget/config mapping | Entry -> SDK -> Download/terminal answer | C9; current error/observer | Leaf, existing SDK | SDK/vendor/credential rewrite |
| `crates/infra-object-storage/src/download.rs` | C10 | Existing public Download; private state/resource/timer declarations | Chunk/collection/Body use one state | C10 | Leaf and existing Tokio/std/futures facilities | Producer queue, transport statuses |
| `crates/infra-messaging/src/registry.rs` | C11 | Existing public registration and private erased handler carry OperationContext | Typed handler invocation | C11 | Leaf, existing event/wire deps | Context on broker wire |
| `crates/infra-messaging/src/consumer.rs` | C11 | Private run_handler fixes context/absolute timeout at existing origin | Delivery -> handler -> existing Outcome | C11; settlement unchanged | Leaf, current Tokio/cancellation | New redelivery/ACK policy |
| `crates/infra-jobs/src/kind.rs` | C12 | Public Job::context accessor, existing projections unchanged | Attempt -> adapter/feature | C12 | Leaf, existing attempt | Engine timeout/claim changes |

Existing proof-file placement is fixed by semantic owner: context tests inline
in the new leaf/HTTP context module; current HTTP harden/authn tests remain
beside those modules; gRPC body/client tests remain inline in `call.rs` and
`client.rs`, mounted proof in `crates/infra-grpc/tests/transport.rs`; auth tests
remain in their current engine modules; cache/outbound/S3 tests remain their
current `src/tests.rs` or inline download tests; OAuth stays `src/tests.rs` and
`crates/infra-oauth2-client-credentials/src/tests/grpc.rs`; messaging keeps registry/consumer tests and
`crates/infra-messaging/tests/jetstream.rs`; jobs keeps `kind.rs`/existing attempt
proof. The deterministic rule for Rust caller adaptation is: modify only an
existing caller whose changed public signature or context propagation is on
C2–C12's described path, in its current file; do not create a new caller module
or broaden business policy. Existing standalone APIs minimize mechanical caller
changes. Any materially new responsibility reopens the affected map row.

## Non-Rust and profile custody

| Path | Exact disposition and owner |
| --- | --- |
| `Cargo.toml`, new `crates/operation-context/Cargo.toml` | Declare one workspace leaf, inherited package/lints and explicit existing Tokio/time and tokio-util/rt features. No versions changed. Workspace `crates/*` already covers membership. |
| `Cargo.lock` | Deliberate minimal update for workspace package/edges; retain every current registry package version/checksum and supported feature intent. No unconstrained regeneration or validation-time mutation. |
| Consumer `Cargo.toml` files | Add leaf to infra-http, infra-grpc, infra-bearerauthn, infra-cache, infra-outbound-http, infra-oauth2-client-credentials, infra-object-storage, infra-messaging and infra-jobs. Use existing optional profile deletion of those crates; add tokio-util or futures-util to S3 only if the selected local mechanism directly uses it, reusing workspace/resolved packages with explicit features. No new external package. |
| `quality/architecture.json` | Register leaf as independent `contract`; add narrow `allow_members` edges for the above consuming members, not broad new role permissions. Existing feature-to-contract allowance makes business carriage legal; do not alter unrelated provider/feature rules. |
| `scripts/lib/template_profiles.json` | Remove stale request-budget mapper marker IDs when relative middleware is replaced; keep RequestDeadline projection markers only where still needed. Add any optional documentation sections to their existing profile marker groups. New context crate and HTTP middleware are unconditional and survive minimum profile. |
| `scripts/ci/changed-surfaces.sh` | Include `crates/operation-context/*` and new HTTP context path in source-template initializer/runtime surface selection; preserve the existing cargo/rust/dependency gates. Update existing classifier self-test for these real new paths. |
| `scripts/tests/template-profile-projections.py`, `scripts/tests/template-candidate-paths.txt` | Extend existing representative retention/projection checks only where the new unconditional carrier or changed markers require them. No new profile dimension or duplicate build matrix. |
| `scripts/lib/template_init.py` | Existing selection of request-budget continues to select optional legacy projection; no new profile or runtime knob. No change unless removal of obsolete marker expectations is mechanically required. |
| `scripts/lib/template_state.py::_batch_blobs` | Completion operating correction: the root routed a mechanically required repair after the existing macOS projection run deadlocked writing the whole Git request before draining output. This same C13 executor may use native subprocess communication and a temporary output file, retaining declared-length blob validation and the existing in-memory result owner. No projection policy, dependency or public interface changes. |
| `template-owned.paths` | No change. `scripts/lib/template_state.py` explicitly protects `crates/` from portable whole-file sync; runtime files remain service-owned. Do not add the new crate to the portable instruction/control manifest. |
| `build/docker/Dockerfile`, `.dockerignore` | No change. Existing planner/source `COPY . .` includes the new crate, and cargo-chef sees workspace members. No special vendor/source COPY needed for a normal workspace member. |
| `docs/architecture/boundaries.md`, `docs/project-structure-and-module-organization.md`, new `docs/operation-budgets.md` | Record neutral leaf, strict dependency edges, public propagation, cancellation and distinct response lifetime. Optional provider examples in the new guide use existing profile markers. |
| `docs/configuration-source-policy.md`, `docs/architecture/integration.md`, `docs/architecture/http.md`, `docs/grpc.md`, `docs/authentication.md`, `docs/cache.md`, `docs/object-storage.md`, `docs/outbound-http.md`, `docs/outbound-machine-authentication.md`, `docs/durable-messaging.md`, `docs/background-jobs.md` | Update each current owner only for C2–C12 behavior/API and accurate retry/physical-call limits; keep established durations and other policy. |
| `api/openapi/service.yaml`, `api/proto/`, `crates/grpc-contracts` | No source or generated edit planned; this task changes local context delivery, not wire schemas. Existing generated checks remain authoritative. |
| `crates/config`, PostgreSQL/migrations/vendor, bootstrap/worker lifecycle | No changes planned. No new default, response reserve, transaction, background service task, provider or startup policy. |

All added Rust files have a present production artifact in C1/C2; no empty
module, trait, directory or framework package is admitted. Final verification
follows the repository route once on the assembled candidate. Nothing in this
map authorizes extra infrastructure, merge or deployment.
