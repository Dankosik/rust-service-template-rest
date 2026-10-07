# Rust ownership map

Status: ready. Owner: Technical Design. Source and mechanism are fixed by
[mechanism](mechanism.md). This map closes placement, not function bodies or a
separate test plan. Existing behavior remains authoritative outside G1/S1/D1.

## Responsibilities

| Responsibility | Affected path / current evidence | Semantic owner and exact action | Dependency, composition and generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| G1 opening custody | `infra-grpc::router::{router,deadline,shed,authenticate,reject}`; auth currently precedes the only semaphore | `infra-grpc`, edit `src/router.rs`: add one private opening middleware/second semaphore and native cooperation in bounded rejection; preserve existing terminal shed | Existing axum/Tokio and observe/status owners; opening lives outside authn-pruned sections, business-only below deadline; no service/generated/auth-provider change | RAII opening permit ends at head/error/cancellation/unwind; existing terminal owner is unchanged | Existing `infra-grpc/tests/transport.rs` for observable router/verifier/health behavior; private router-local tests only if a deterministic internal boundary is needed | New admission policy, queue, global quota, or inability to preserve terminal ownership returns to Design/Definition |
| S1 pre-header original end | `ObjectStorage::get`, existing PUT absolute-end idiom and `Inner.operation_timeout` | `infra-object-storage`, edit `src/lib.rs`: capture end before work, check before send polling and after results/metadata, transfer it with existing resources into Download and verify empty EOF | Existing public get signature and AWS builder; no provider/signing/retry policy or new interceptor; no config source change | get future owns resources until atomic handoff; cancellation drops them | Existing `infra-object-storage/src/tests.rs` request/mock-client fixtures; Download-local coverage for handoff contracts as needed | SDK behavior defeating cancellation or new public deadline argument returns to Design/Definition |
| S1 returned body custody | Existing `Download`, `DownloadState`, `End`, `poll_chunk`, Body and bytes in `src/download.rs`; existing gRPC `call.rs` is pattern evidence | `infra-object-storage`, reshape private state in the same `src/download.rs`: active resource bundle, one terminal transition, original end, Weak timer/exit guard/owned JoinHandle, native cooperative polling and failure hint with unknown upper bound | `Download`, metadata(), next_chunk(), bytes(), Body signatures stay public and unchanged; constructor stays `pub(crate)` with end parameter; all new lifetime state/guards/helpers private; no infra-grpc dependency or generic common module | One terminal extraction releases active body, last chunk on failure, observation and permit outside mutex; success retains only validated final chunk; synchronous Drop aborts timer; handle/completion proof checks actual exit | Private `#[cfg(test)]` beside Download for lifetime/body/clock/drop ownership; existing crate fixtures for public consumers, including HTTP/1 handoff after autonomous failure; existing emulator/conformance remain provider proof owners | Unpolled resource retention, non-cooperative adapter loop, new task family or SDK limits invalidate Design; new behavior/dependency goes to Definition |
| D1 typed source comments | Existing `Limits` comments in router.rs, `Options.operation_timeout`/get documentation in storage lib.rs, config fields in `crates/config/src/grpc.rs` and `object_storage.rs` | Current respective owners; describe two independent K counts and GET original end through EOF. Values, keys, conversion, ranges and validation stay unchanged | Config remains immutable source owner; no generated/protobuf/OpenAPI surface | No new lifetime | Static consistency under existing docs/config evidence; do not add config tests that merely mirror changed comments | A requested new value/key/validation rule returns to Definition |
| D1 operator/resource scope | Existing gRPC/storage guides, decisions, configuration policy, runtime/integration leaves and production contract | Existing docs listed below; replace superseded promises, retain scoped capacity and consumer-owned class/backlog decisions through canonical links | Existing profile markers and manifest own pruning in profile-bearing files; production-contract uses the always-retained link rule below and needs no marker; no new profile/CI gate or generated-source edit | Guidance names operation-owned timer and finite active custody without a new process service | Static guide/source review and applicable existing profile projection checks selected by final validation | Any contradiction requiring business capacity, provider contract or new rollout behavior returns to Definition |

### Nonmechanical reuse decisions

- G1 source is existing owner plus existing Tokio Semaphore (resolved 1.53.1),
  adding one responsibility-specific layer. Strongest rejected source is Tower
  concurrency/load-shed composition; it cannot remove the gRPC-specific
  rejection/terminal owners. Parity is existing terminal/health/zero/deadline
  coverage plus G1 verifier-entry falsifiers. Upgrade when native gRPC-aware
  admission deletes the same glue without changing the accepted lifetimes.
- S1 shared custody reuses the existing `infra-grpc/src/call.rs` ownership shape
  from the fixed source with standard Arc/Mutex and resolved Tokio native timer,
  handle and cooperative polling. It does not copy that transport's status,
  generic Body abstraction or Observation owner. Strongest rejected source is
  SDK timeout/stall configuration, which lacks returned-body custody and cannot
  free adapter-held chunks/permits unpolled. Parity is existing exact-length,
  checksum and stable-error behavior plus S1 timer/drop/finality falsifiers.
  Replace local custody only when upstream provides that whole lifecycle, not
  because another wrapper offers a read timeout.
- No new crate, source module, trait, dependency, feature or directory is
  needed in product code. Removing the new opening middleware would remove
  pre-auth admission; removing shared active state/Weak timer would remove
  unpolled cleanup; removing the exit guard would leave unexpected timer exit
  without a resource/failure owner. Each has a current constraint. No ownership
  fork survives within the existing crate graph, so placement self-review is
  sufficient; the independent Technical Design Review includes coherence.
  The complementary Rust Ownership panel is untriggered: several existing
  files change, but no crate or cross-crate responsibility moves.

## Files (inverse map for all expected Rust changes)

| Path | Responsibilities | Present reason / declarations and visibility | Call-path role | Lifecycle/error owner | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- | --- |
| `crates/infra-grpc/src/router.rs` | G1 opening custody; D1 typed source comments | Existing router, Limits, private middleware and rejection functions; one new private opening function only | Composed business ingress outside auth, inside original deadline | Scoped opening permit; existing tonic Failure/record_shed; existing terminal handoff unchanged | Existing axum/Tokio/authn conditional imports and crate observe/call/status | Provider quotas, terminal-body rewrite, business authorization or new generic limiter |
| `crates/infra-grpc/tests/transport.rs` | G1 proof | Existing black-box transport fixtures and tests; new tests remain private | Drive real composed router/listener and verifier fixture | Tests cancel/join owned work; production behavior uses G1 owner | Current dev dependencies and authn markers | A new runner, external provider or test-only production policy |
| `crates/infra-object-storage/src/lib.rs` | S1 pre-header original end; D1 typed source comments | Existing public ObjectStorage API and private async/polling boundary; no new exported type/method | get to SDK send to Download::open | Original deadline/cancellation until custody handoff; existing OperationGuard/fail mapping | Existing AWS/Tokio/bytes and sibling download/observe/error | Body lifetime duplicated beside Download, new retryer/provider path, #248 interceptors |
| `crates/infra-object-storage/src/download.rs` | S1 returned body custody | Existing public Download and Body impl; private State/Resources/terminal representation/timer-exit guard and helpers; local test module if needed | Every next_chunk/Body/bytes path shares one poll/terminal boundary | Mutex chooses once, extraction finalizes outside lock, timer is operation-owned | Standard library and already-declared Tokio/AWS/bytes/http-body plus sibling observe/error | Process supervision, common transport framework, producer queue, allocation rewrite from #248 |
| `crates/infra-object-storage/src/tests.rs` | S1 pre-header/public-consumer proof | Existing fixture module; private tests/fixtures | Drive ObjectStorage get with existing mock SDK responses | Tests own clocks, fake bodies and joined task lifetimes | Current crate dev dependencies | Live-provider claim, new external harness, test-only production seam when existing ByteStream constructors suffice |
| `crates/config/src/grpc.rs` | D1 typed source comments | Documentation of max_in_flight field only | Typed config to unchanged Limits conversion | No changed resource/error behavior | Existing config dependencies | New knob, default or range change |
| `crates/config/src/object_storage.rs` | D1 typed source comments | Documentation of operation_timeout field only | Typed config to unchanged Options | No changed resource/error behavior | Existing config dependencies | New knob, default or range change |

Proof placement rule: implementations choose cases and may use owner-local
`#[cfg(test)]` sections in the already-listed production files where private
custody/drop state must be exercised. Public mock-SDK flow cases use existing
`src/tests.rs`; gRPC composed transport behavior uses existing `tests/transport.rs`.
No alternate test directory or production test seam is selected. Tests and
fixtures not needed for a discriminating invariant remain unchanged.

## Non-Rust files and projection custody

| Existing file | Required current change / containment |
| --- | --- |
| `docs/grpc.md` and `docs/grpc-decisions.md` | Describe pre-auth K independent of authenticated terminal K, bounded rejection/deadline precedence, health and zero; replace current authentication-before-only-cap explanation without undoing terminal semantics. Whole files are gRPC profile-owned; auth-specific prose remains in existing authn markers. |
| `docs/object-storage.md` and `docs/object-storage-decisions.md` | Replace indefinite Download custody and headers-only GET decision; original total GET budget, unpolled release, stable EOF/failure, slow-reader migration to presign/adequate current timeout. Preserve #248 collision boundary. Existing object-storage marker/whole-file removal applies. |
| `docs/configuration-source-policy.md` | Update existing gRPC/object-storage budget sections; retain keys/defaults/ranges/precedence. Keep content within current profile markers. |
| `docs/architecture/runtime-lifecycle.md` | Existing object-storage lifecycle section explains operation-owned timer cancellation/drop with no new shutdown stage; existing marker. |
| `docs/architecture/integration.md` | Existing object-storage boundary names complete GET custody alongside existing size/concurrency/failure ownership; existing marker. |
| `docs/production-contract.md` | Consolidate D1 resource-scope obligations as conditional requirements when a service retains/adopts the capability. This always-retained file has no profile markers. Use only file-level links to `configuration-source-policy.md`, `architecture/integration.md`, `architecture/runtime-lifecycle.md` and `architecture/persistence.md`; those retained owners already contain the appropriately marked optional-guide links. Add neither direct links to removable guides nor fragment links to removable sections, and add no markers to this file. No guessed capacity values, new business classes or quotas. |
| `scripts/lib/template_profiles.json` | No D1 documentation registration is needed: production-contract stays unmarked and links only retained owner files. Only if a new auth-only proving section needs a marker, register it in the existing authn block list. Default is reuse existing proving sections with no manifest edit. The parser requires every executable marker to match the inventory exactly, so do not insert unregistered markers. |

`env/config/local.toml` contains no conflicting behavior promise requiring a
change; values remain unchanged. Generated protobuf/OpenAPI, `call.rs`, auth
providers, storage `body.rs`/`observe.rs`/`error.rs`, Cargo manifests/lockfile and
bootstrap are read-only parity authorities for this change. If implementation
needs one of them to change materially, return the exact affected invariant to
Technical Design rather than silently expanding the map.

The documentation repair closes Planning finding F1. Current manifest inspection
confirms all four production-contract target files above survive profile
projection, while authentication, outbound HTTP/auth, jobs/webhook, messaging,
gRPC and storage guide files are removable. Existing target owners retain their
own marker/inventory rules. This avoids widening marker ownership merely to add
links; it does not remove any D1 obligation or claim an optional capability is
active in an unselected profile. Relative file links survive even when a target
owner's capability-specific section is removed.
