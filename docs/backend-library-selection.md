# Backend library selection

Use this guide when a feature needs a new dependency or starts repeating a
mechanical implementation. `Cargo.toml` and `Cargo.lock` own declared versions,
enabled features, and resolved crates; this document records only benchmark
evidence, non-code rejection reasons, and selection criteria for capabilities
not yet adopted.

A dependency earns its place by deleting a current mechanism or a manually
maintained relationship. Prefer the standard library and existing workspace
dependencies first. Declare versions once in `[workspace.dependencies]` with
`default-features = false`, enable only needed features in consuming crates, and
recheck maintenance, licensing, and advisories at adoption time.

## Rejected utility and hot-path candidates

- **HTTP / header / MIME helpers:** `mime` in `shared-rules` (violates `std`-only charter and rejects parameters RFC 9110 provider checks accept); `headers` for `Bearer` / `Retry-After` (looser than RFC 6750 and lacks `Retry-After` value parsing).
- **Concurrency, clocks, and collections:** `quanta`, `recloser`, and `failsafe` (clocks ignore `tokio::time::pause`); `arc-swap` on JWKS refresh (already serializes off the hot path behind `moka` memoization); `indexmap` on tiny static registries (`infra-jobs` `Kinds::get`: `Vec` `8.3 ns` vs `IndexMap` `18.0 ns` at 8 kinds, `12.2 ns` vs `18.5 ns` at 16 kinds); `atoi` (`7.0 ns` vs `7.6 ns` against `str::parse` on validated ASCII digits — `< 1 ns` per RPC does not justify an extra crate) and `itoa` (`std::fmt::NumBuffer` formats integers on the stack in `std`); `smol_str`, `dashmap`, `papaya`, and `phf` (covered by `compact_str`, `arc-swap` + `foldhash`, and `enum-map` without duplicate abstractions); `wait-timeout` (installs a process-wide `SIGCHLD` handler alongside `tokio::process`).
- **Serialization and config:** `sonic-rs` and `simd-json` (replace the JSON contract rather than one mechanism); `serde_with::OneOrMany` / `PickFirst` in config (error messages can echo secret/raw values); `deranged` in config (turns structured validation errors into decode errors); `strum` on failure/metric enums (wire/metric spellings intentionally differ from variant names).
- **Testing and observability:** `tracing-test` (global rendered-text matcher cannot isolate concurrent structured span/field assertions); `tracing-capture` and `prometheus-parse` (unmaintained upstream); `openmetrics-parser` (LGPL-3.0); `tokio-test` (`futures_util::poll!` suffices).

### Narrow hot-path and safety primitives

Measured on a dedicated 4-vCPU Xeon 8280 DigitalOcean c-4 (`linux/amd64`, Debian 13, `rustc 1.100.0`, fat LTO, `codegen-units = 1`, `jemalloc`, 15 interleaved rounds):

| Mechanism | Before | After | Scope boundary |
| --- | ---: | ---: | --- |
| 4-thread metric-registry lookup (`StdRwLock<HashMap>` → `arc-swap::ArcSwap`) | 314 ns | 44.4 ns | Read-mostly route/method registries in `infra-http` and `infra-grpc` (`380 ns` → `196 ns` with per-request work); writers serialize under `parking_lot::Mutex<()>`. |
| 4-thread shared-handle read (`parking_lot::Mutex` → `arc-swap::ArcSwap` / `ArcSwapOption`) | 694–789 ns | 241–319 ns | `infra-serviceauth` cached `HeaderValue` (`694 ns` → `241 ns`) and `infra-cache` connection `State` (`789 ns` → `319 ns`) under realistic per-request work; transitions serialize under `parking_lot::Mutex<()>`. |
| HTTP active-request gauge (`StdRwLock<HashMap<Method, _>>` → `EnumMap<MethodSlot, OnceLock<_>>`) | 28.0 ns | 2.04 ns | Closed HTTP method set in `infra-http`. |
| ServiceAuth caller lookup (`BTreeMap` → `IndexMap<_, _, foldhash::quality::RandomState>`) | 17.1 ns | 12.0 ns | Sorted `O(1)` caller registry in `infra-serviceauth`. |
| Outbound gRPC timeout encode (`tonic::Request::set_timeout` → stack `std::fmt::NumBuffer`) | 244 ns | 58.7 ns | Stack-formatted `grpc-timeout` metadata in `infra-grpc` (plus integer formatting in `infra-messaging` and `infra-outbound-http`). |
| Short diagnostic & span strings (`String` / `format!` → `compact_str::CompactString`) | 7.35–72.1 ns | 2.20–30.2 ns | `7.35 ns` → `2.20 ns` (`const_new` `'static`), `8.44 ns` → `2.55 ns` (runtime `≤ 24` B), and `72.1 ns` → `30.2 ns` (`format_compact!` HTTP span names) across `health::ProbeError`, `infra-http::Problem`, and `infra-object-storage`. |
| Scratch buffers & small header slices (`Vec` → `smallvec::SmallVec`) | 51.2–79.6 ns | 35.4–37.4 ns | `79.6 ns` → `35.4 ns` on `256` B JSON log scratch (`infra-telemetry`), `51.2 ns` → `37.4 ns` on HTTP idempotency headers, plus `infra-jobs` SQL bind slices. |
| OAuth2 client-assertion `AssertionClaims` JSON (`getrandom` + `String` → `uuid/fast-rng` + `Uuid`) | 317 ns | 216 ns | Isolated end-to-end `serde_json::to_vec` serialization in `infra-oauth2-client-credentials` (`ChaCha12` CSPRNG, zero intermediate `String`). |
| One-shot gRPC response handoff (`Arc<StdMutex<Option<T>>>` → `Arc<atomic_take::AtomicTake<T>>`) | 42.0 ns | 33.7 ns | Single-producer single-consumer response/outcome cell in `infra-grpc`. |
| Short non-async critical sections (`std::sync::Mutex` → `parking_lot::Mutex`) | 14.2 ns | 12.5 ns | Synchronous state locks on Linux `futex`, eliminating `PoisonError::into_inner` recovery boilerplate across infrastructure crates (`std::sync::Mutex` stays only behind `#[cfg(feature = "hotpath")]`). |
| Telemetry background writer idle 1 s window (`mpsc::recv_timeout(10ms)` → `crossbeam_channel::select_biased!`) | 98 wakeups/s | 0 wakeups/s | Eliminates idle timer context switches (`99` → `1` voluntary context switch) while flushing dirty buffers after `10 ms`. |
| Transient secret buffers (`String` / `Vec<u8>` → `zeroize::Zeroizing`) | — | scrubbed on drop | Decoded webhook HMAC keys, OAuth2 access tokens / client assertions, service-auth Bearer buffers, and rotated cache/PostgreSQL password-file buffers alongside `secrecy` at config boundaries. |

Core-pinned end-to-end Axum HTTP stack on the same `c-4` host (`2 vCPU` server / `2 vCPU` client, 7 interleaved 6 s rounds, 50% `200 OK` JSON + 50% `400 Problem` RFC 9457): `97,630 req/s` (`20.11 µs` CPU/req, `p50 = 317 µs`, `p99 = 664 µs`, `21.0 MB` RSS) → `119,030 req/s` (`16.72 µs` CPU/req, `p50 = 269 µs`, `p99 = 424 µs`, `21.2 MB` RSS; `+21.9%` throughput, `−16.9%` CPU/req, `−36.1%` p99).

<!-- template:begin oidc-jwt:docs-library-selection-bearer-jwt-memo -->
### Verified bearer JWT memo

| Operation | Before | After |
| --- | ---: | ---: |
| Bearer JWT verified again, RS256 | 21.7 µs (9.8–42.2) | 216 ns (102–497) |
| Bearer JWT verified again, ES256 | 100.0 µs (33.5–134.5) | 248 ns (91–494) |

First-verification insert overhead stayed within measurement noise. Moka bounds
retained entries up to the remaining `exp` window, and any JWKS refresh drops
prior memoized entries so retired keys never authorize cached tokens. Reopen if
tokens are predominantly single-use or verification depends on request inputs
beyond the token, key set, and clock.
<!-- template:end oidc-jwt:docs-library-selection-bearer-jwt-memo -->

### Global allocator

Measured on a 4-vCPU Xeon 8280 DigitalOcean c-4 (`linux/amd64`, fat LTO, 5
interleaved 20 s rounds plus a 30-minute mixed soak against PostgreSQL):

| Workload | CPU per request, glibc | mimalloc | jemalloc |
| --- | ---: | ---: | ---: |
| 1 KiB JSON | 65.8 µs | −20% | −19% |
| 64 KiB JSON | 442 µs | −4% | −1% |
| Indexed read | 158 µs | −18% | −20% |
| Durable write | 152 µs | −14% | −15% |
| Mixed, 30 minutes | 219 µs | −13% | −12% |

| Mixed 30-minute soak | glibc | mimalloc | jemalloc |
| --- | ---: | ---: | ---: |
| Throughput | 9,027/s | +16% | +15% |
| p99 latency | 13.8 ms | 11.9 ms | 12.0 ms |
| Resident memory under load | 18.7 MB | 30.5 MB | 23.3 MB |
| Resident memory after 60 s idle | 18.1 MB | 28.0 MB | 17.6 MB |

`infra-jobs` soak benchmarks (20 minutes at 1,000 jobs/s, 64 workers) showed the
same pattern (−3% to −11% worker CPU and 79.5 MB post-soak RSS on jemalloc vs
98.5 MB on mimalloc). Both long-running binaries (`service` and `jobs-worker`)
select jemalloc because it matches mimalloc's CPU reduction while returning idle
pages to the OS.

### Open-file limit

Docker (since Engine 25), containerd (since 1.8), and systemd (since 240) start
containers with a soft `RLIMIT_NOFILE` of 1,024 under a much higher hard limit
for legacy `select(2)` compatibility. Without raising the soft limit to the hard
limit at startup (`rlimit::increase_nofile_limit(u64::MAX)`, matching the Go and
Deno runtimes), socket accepts fail with `EMFILE` around 1,000 connections
regardless of listener configuration. `rlimit` handles macOS's
`kern.maxfilesperproc` cap without adding a crate beyond `metrics-process`'s
existing dependency tree; the platform still owns the hard limit.

## Request DTO validation

Choose one validation crate per service (`validator` for flat REST DTOs, or
`garde` when nested/enum/context rules dominate) and verify trait compatibility
before pairing with third-party extractors such as `axum-valid`:

- Map validation failures into the closed `Problem` catalog (`invalid_params`
  with RFC 6901 JSON pointers) without turning body-limit rejections into
  validation errors.
- Build client-visible reasons from allow-listed constraint descriptions; **never
  serialize the validation crate's raw error struct or submitted field values**
  (`validator` embeds raw input values in error parameters).
- Keep `serde`, runtime validation, and `utoipa` schemas aligned with explicit
  accepted/rejected boundary tests.

## Serialization, builders, and persistence

| Trigger | Candidate | Boundary to preserve |
| --- | --- | --- |
| Nonstandard wire formats or `PATCH` tri-state (`missing` vs `null` vs value) | `serde_with` (`double_option` for tri-state) | Explicit wire contract, unknown-field rejection, and OpenAPI schema parity; keep `humantime-serde` and `bytesize` where already used. |
| Repeated mechanical newtype traits | `derive_more` (narrow derives only) | Keep checked `TryFrom` invariants and `thiserror` for errors; never derive unchecked `From<String>` or mutable deref. |
| Complex constructors with optional inputs | `bon` (or `typed-builder`; `derive_builder` only when runtime fallible `build()` is the contract) | Install at most one builder crate; never expose setters that bypass `Problem::new(code)` invariants. |
| First feature-owned persistence repository | SQLx (`FromRow`, `query!` with committed `.sqlx` metadata) | Evaluate `SeaORM` only for demonstrably CRUD-heavy services that preserve DSN admission, migrations, and `CommitUnknown` handling without a second schema owner. |

## Outbound integrations and stateful capabilities

| Capability | Direction / Candidate | Required boundary |
| --- | --- | --- |
| Retry mechanics | `backon` | Caller owns idempotency, retryable error class, jitter, and outer deadline; never retry `CommitUnknown` or stack retry loops. |
| Process-local cache | `moka` | Bounded capacity and TTL; not cross-replica state. |
<!-- template:begin cache:docs-library-selection-cache -->
| Distributed cache | `infra-cache` (`redis`) | Multiplexed bytes-only cache with fallback to source of truth; see [Cache decisions](decisions/cache-decisions.md). |
<!-- template:end cache:docs-library-selection-cache -->
<!-- template:begin object-storage:docs-library-selection-object-storage -->
| S3-compatible object storage | `infra-object-storage` (`aws-sdk-s3` + `aws-config`, default features off) | Pinned `BehaviorVersion`, explicit credentials/workload identity, single-PUT bounds; see [Object storage decisions](decisions/object-storage-decisions.md). |
<!-- template:end object-storage:docs-library-selection-object-storage -->
<!-- template:begin service-auth:docs-library-selection-service-auth -->
| Caller-signed gRPC service tokens | `infra-serviceauth` (`jsonwebtoken` + `aws-lc-rs`) | Single crypto family (`aws-lc-rs`), verified-token memoization; see [Service authentication decisions](decisions/service-authentication-decisions.md). |
<!-- template:end service-auth:docs-library-selection-service-auth -->
| Outbound HTTP contract tests / snapshots / property tests | `wiremock` / `insta` / `proptest` | Dev-dependencies only; never replace inbound router tests or duplicate committed OpenAPI snapshots. |
<!-- template:begin jobs:docs-library-selection-jobs -->
| Durable PostgreSQL background jobs | `infra-jobs` | Transactional `in_tx` enqueue and idempotent handlers; reassess external queue crates before replacing the engine. |
<!-- template:end jobs:docs-library-selection-jobs -->
<!-- template:begin messaging:docs-library-selection-messaging -->
| Durable JetStream messaging | `infra-messaging` (`async-nats` with `aws-lc-rs` and `nkeys`) + `domain-events` | Keep `ring` off so the workspace shares one Rustls provider; `utoipa` owns AsyncAPI payload schemas and `domain-events` keeps features provider-free. |
<!-- template:end messaging:docs-library-selection-messaging -->

<!-- template:begin messaging:docs-library-selection-messaging-rejection -->
For messaging, reject `asyncapi-rust` / `schemars` (introduces a second schema
derive alongside `utoipa` without `time` parity) and `cloudevents-sdk` (conflicts
with the pinned `async-nats` major line). `time/large-dates` stays enabled so
Go-accepted RFC 3339 timestamps with year-9999 offsets normalize to UTC year
10000 without a custom parser.
<!-- template:end messaging:docs-library-selection-messaging-rejection -->
<!-- template:begin cache:docs-library-selection-cache-rejection -->
For cache, reject `fred`, connection pools (`deadpool-redis`, `bb8-redis`), and
`testcontainers` alongside Compose Valkey ([Cache decisions](decisions/cache-decisions.md)).
<!-- template:end cache:docs-library-selection-cache-rejection -->
<!-- template:begin object-storage:docs-library-selection-object-storage-rejection -->
For object storage, reject `object_store`, `opendal`, `rust-s3`, `minio`,
`aws-sdk-s3-transfer-manager`, and default AWS SDK features ([Object storage decisions](decisions/object-storage-decisions.md)).
<!-- template:end object-storage:docs-library-selection-object-storage-rejection -->

## Bounded stdout writer

`crates/infra-telemetry` uses bounded `crossbeam_channel` queues and one owned
writer thread rather than third-party async loggers:

| Candidate | Deciding limitation |
| --- | --- |
| Synchronous stdout at event site | Stalled stdout blocks Tokio worker threads. |
| `tracing-appender` | Drops sink I/O errors, acknowledges before final flush, and uses fixed blocking waits in `Drop`. |
| `flexi_logger` | Async mode uses an unbounded channel (`pool_capa` bounds buffer reuse, not queued records) and lacks caller-deadline drop-newest semantics. |
| `std::sync::mpsc::sync_channel` | Lacks multi-channel selection, forcing `recv_timeout` polling (~84–100 idle wakeups/s) to observe shutdown alongside records; `crossbeam_channel::select_biased!` blocks with `0` idle wakeups/s and only arms the flush timer while buffered output is dirty. |
| `tokio::sync::mpsc` + thread | Adds runtime coupling where `crossbeam_channel::Sender::try_send` already distinguishes `Full` vs `Disconnected` and detaches cleanly on timed-out shutdown. |

Reopen when `std` or a maintained crate satisfies zero-idle-wakeup multi-source
selection, nonblocking drop-newest admission, observable sink write/flush
failures, and caller-bounded shutdown without blocking destructors.
