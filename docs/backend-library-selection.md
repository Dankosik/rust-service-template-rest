# Backend library selection

Use this guide when a feature needs a new dependency or starts repeating a
mechanical implementation. It records the boilerplate-reduction decisions,
not a bundle of dependencies every service must install. The
[upstream research synthesis](https://github.com/Dankosik/rust-service-template-rest/blob/81bbd16b320c90d430522e69fc2d52a2aaa049de/specs/boilerplate-reduction/research/synthesis.md)
records the inspected baseline and the evidence behind the immediate choices.

A dependency earns its place by deleting a current mechanism or a manually
maintained relationship. Prefer the standard library and existing dependencies
first. Declare a version once in the workspace, disable default features,
enable only the needed features in the consuming crate, and commit a
Cargo-generated lockfile. Recheck compatibility, maintenance, licensing and
advisories at adoption time; a recommendation is not a permanent approval of
all future versions. Do not hand-edit checksums or weaken dependency gates.

## Production utility toolkit

The recipes are normal tests in the existing test package; only production
consumers add normal dependencies. `Probe` now uses `async-trait`, cancellation
uses `tokio-util`, and the `service-failure` catalog is declared once by its
`codes!` macro, with `Display` and `Serialize` delegating to `as_str`.

## Adopted for existing code

| Library | Owner and use | Boundary |
| --- | --- | --- |
| `axum-test`, no optional features | `infra-http` dev-dependency: ordinary router requests, response decoding and assertions. | Use in-process mock transport. Keep raw `Request<Body>` and `oneshot` where byte framing, streaming body limits, concurrency or response extensions are the subject; keep real server/process tests for connection and lifecycle behavior. |
| `rstest`, no default features | `service-config` and `service` dev-dependency: named parameterized cases for range checks, addresses and the OpenAPI security classifier. | Keep semantic expectations visible in each case. Do not build a fixture framework for simple values or hide lifecycle setup. |
| `tikv-jemallocator` 0.7, no default features | `service` and `jobs-worker` binaries: the global allocator. | Only the long-running binaries. `migrate` and `openapi` run for seconds and keep the system allocator. Libraries never set an allocator. The image builder installs `make` because jemalloc builds with `configure` and `make`. |
| `rlimit` 0.11, no default features | `service` and `jobs-worker` binaries: raise the open-file soft limit to the hard limit at startup. | See [Open-file limit](#open-file-limit). A failure is logged as `open_file_limit_unchanged` and startup continues. |
| `derive_more` `debug`/`display` | Debug that prints a fixed string (`X([REDACTED])`, `X { .. }`) and one-field Display, through a struct-level `#[debug("...")]` or `#[display("...")]`. | A Debug that names selected fields stays hand-written: `#[debug(skip)]` is a denylist, so a field added later would print. |
| `enum-map` 3.1, `derive` | Counters and histograms indexed by an enum in `infra-telemetry`, `infra-object-storage`, `infra-messaging` and `infra-cache`; the per-operation failure flags of `infra-jobs`; the per-reason refusal counters of `infra-serviceauth`. | Replaces hand-kept `ALL`/`COUNT` tables, one field per variant and their ordering asserts. Statics stay const-initialised through `EnumMap::from_array`; handles registered on first use sit in `OnceLock` cells, so the scrape gains no zero-valued series. Not for `tonic::Code` or the `codes!` catalog. |
| `const-hex` 1.19 | Lowercase hex of digests in `infra-http`, `infra-messaging` and the log formatter. | Chosen over `hex` 0.4.3, unreleased since 2021. Hot paths encode into a stack buffer; output is a wire and storage contract and must stay lowercase. |
| `derive-where` 1.7 | `Clone`, `Debug` and `Default` on generic gRPC types without bounds on their parameters. | Only where a std derive would add a bound the type does not need. |
| `async-stream` 0.3 | The gRPC health `Watch` stream and test streams. | Not for per-row database streams or `Body` impls, which own polling budgets. |
| `moka` `sync` | The verified-token memos in `infra-serviceauth` and in the JWT engine of `infra-bearerauthn`, each bounded at 1024 entries. | The validity-window check stays in the verifier and runs on every hit; the cache only bounds capacity and time. See [Verified bearer JWT memo](#verified-bearer-jwt-memo). |
| `similar` 2.7, `text` (dev) | The unified diff in the committed-OpenAPI test. | Kept at the 2.x line `insta` already resolves. |
| std `assert_matches!` | Pattern assertions in tests, which print the value on failure. | Std since Rust 1.96; no crate. |
| `foldhash` 0.2, `std` | The hasher of the verified-token memos in `infra-serviceauth` and `infra-bearerauthn` (`quality`), and of the per-call method tables in `infra-grpc`, the endpoint tables of inbound webhooks and the per-route metric handles of `infra-http` (`foldhash::HashMap`). | Only for keys the service admitted: a memo entry exists only after its signature verified, and server request paths are looked up in tables fixed at startup. The gRPC client admits validated method paths on first use into a bounded table. Measured on 2026-11-12 (Apple M-series, release, fat LTO, median of seven runs): hashing a 420-byte token 70.2 → 8.2 ns, a memo hit 105.5 → 43.6 ns, a 46-byte path lookup 8.9 → 3.5 ns. hashbrown already resolves it, so it adds no package. Digest-keyed caches such as introspection keep the default hasher; a 32-byte key saves a few nanoseconds beside a SHA-256 of the token. |
| `arrayvec` 0.7 | The captured-field tables of the log formatter. | Replaces a sentinel-filled array and a separate count. A new table is no longer initialised in full: measured the same way, a span with two fields 405 → 335 ns to create and an event under one span 2,138 → 2,088 ns. The value buffer stays the local `Bytes`, whose writes are all-or-nothing; `ArrayVec`'s `io::Write` writes a prefix. `ArrayString` holds the decimal timestamp of a webhook signature. |
| `metrics-util` 0.20, `debugging` (dev) | `DebuggingRecorder` in the `infra-http` and `infra-webhooks` tests that assert which series a route registers. | Replaces three hand-written recorders. A snapshot resets counters and gauges, so a test that reads a gauge repeatedly or asserts the order of increments keeps its own recorder (`infra-jobs`, `infra-bearerauthn`, the jobs wake test). Adds one test-only package, `ordered-float`. |
| `opentelemetry-proto` 0.33 and `prost` (dev) | Tests decode captured OTLP export bodies into the generated trace messages: `infra-telemetry` and the gRPC process test of `service`. | Replaces hand-encoded protobuf tags and bare byte runs that could match anywhere in a body. A raw scan remains only where the point is that a private value appears nowhere. `opentelemetry-otlp` already resolves it, and it moves with the other OpenTelemetry pins. |
| tokio `watch::Receiver::wait_for`, tokio-util `AbortOnDropHandle` | The accept-failure and background-failure waits; the accept loop of `infra-http` and the consumer supervisor of `infra-messaging`. | Replace hand-written wait loops and `abort()` calls in `Drop`. `Deadline::wait` in `operation-context` keeps its expired-first loop: a timer alone leaves a zero budget pending on its first poll. |
| `async_nats::Subject` | The subject of a route and of a prepared event in `infra-messaging`. | A publish no longer copies the subject three times; the bytes on the wire are the same. |
| `proptest` with `regex` as the oracle (dev) | Property tests for hand-written grammars (request id, service name, key id, JWT type, job kind, message identity, schema label), the idempotency key and replay allow-list, the webhook signing frame and the readiness failure threshold. | 64 to 256 cases each. The oracle is the documented pattern or an independent model, never the implementation. |

Rejected after the same code search, 2026-11-12: `mime`, because
`shared_rules::rfc9110::is_json_content_type` owns the JSON media-type check
and `Mime::from_str` would refuse parameters that rule accepts; `tracing-test`,
because it matches rendered text under one global subscriber and the capture
fixtures assert structured fields, span records and background-task events;
`tracing-capture` 0.1.0, unreleased since 2024-03 and pulling a second
`itertools`; `headers` for Bearer and Retry-After, being looser than RFC 6750
and unable to read a Retry-After value. Crates with no matching code
(identifier, slug, phone, currency, HTML, User-Agent, CLI output, `sd-notify`,
`rust-embed`) wait for a feature that needs them.

Rejected in the narrow-crate search of the same day: `smallvec` and
`compact_str` for token scopes and claims, built once per JWT next to a
signature check, exposed as `&[String]`, and mostly longer than 24 bytes;
`arc-swap` for the JWKS key set, whose keys and refresh counters change under
one `watch` lock that decides who starts a refresh (reopen on observed
contention there); `quanta`, whose clock ignores `tokio::time::pause`;
`pin-project-lite`, since the wrapped bodies are `Unpin`; `scopeguard`, since
the drop guards carry metrics and state; `memchr`, whose candidate searches are
in test fixtures; `atoi`, `itoa` and `parking_lot`, with no measurable site;
`base64-simd`, unreleased since 2023; `sonic-rs` and `simd-json`, which would
replace the JSON contract rather than one mechanism.

Rejected in the wide search of the same day (fourteen read-only passes over
the code and the crate catalogs): `fastrand` for jitter, already decided in
[Outbound machine authentication](decisions/outbound-machine-authentication-decisions.md);
`parking_lot` for the 31 `unwrap_or_else(PoisonError::into_inner)` sites, a
readability gain only (reopen when `std::sync::nonpoison` is stable; it is not
on Rust 1.99); `wait-timeout`, which installs a process-wide `SIGCHLD` handler
beside `tokio::process`; `tokio-test`, since `futures_util::poll!` covers
polling once; `async-channel` for the log queue, two new packages for what an
`RwLock` already removed; a `BufWriter` over stdout, which would delay log
lines by the idle window and lose buffered lines on `SIGKILL`; `strum` for
metric-label functions, whose text is a contract that differs from several
variant names; `serde_with::OneOrMany` and `PickFirst` in configuration, whose
errors can quote the value; `deranged` for configuration ranges, which would
move a validation failure to a decode failure with other text; the async-nats
`nuid` feature, a process-wide mutex with no measurement; `arc-swap` for the
gRPC client label table, `atomic-take`, `rusty-fork`, `smart-default`,
`nutype`, `httparse`, `pretty_assertions`, `simdutf8`, `dashmap`, `papaya`,
`phf`, `smol_str` and `v_jsonescape`, with no site that pays for them;
`prometheus-parse`, unreleased since 2023-12, and `openmetrics-parser`,
LGPL-3.0; `recloser` and `failsafe`, whose clocks ignore `tokio::time::pause`.
`cargo-hack`, `typos`, `cargo-pgo` and `cargo-about` are open: the first two
add a gate to every pull request, the third needs a training workload the
repository does not have, the fourth is a licence decision.

The examples live with their owners in `crates/infra-http/src/harden/tests.rs`,
`crates/config/src/validate.rs` and `crates/service/tests/openapi.rs`. Test-only
libraries do not become normal dependencies of production crates.

### Hot-path measurements of 2026-11-12

Each change below was kept only after its own before-and-after measurement:
Apple M-series, release, fat LTO, medians of seven to nine alternating rounds
of two saved binaries. The workstation was building other crates throughout,
so absolute values moved by a factor of two or more between rounds; the range
of the rounds follows each median.

| Operation | Before | After |
| --- | ---: | ---: |
| Bearer JWT verified again, RS256 | 21.7 µs (9.8–42.2) | 216 ns (102–497) |
| Bearer JWT verified again, ES256 | 100.0 µs (33.5–134.5) | 248 ns (91–494) |
| HTTP request duration and active-request gauge recorded | 3,118 ns (1,487–5,039) | 515 ns (380–917) |
| gRPC outcome of a rejected call that carries details | 1,430 ns (1,111–2,000) | 635 ns (504–739) |
| Refused service token counted | 136 ns (89–145) | 2.6 ns (1.9–3.0) |
| Log span with two fields created | 1,054 ns (583–1,187) | 569 ns (209–770) |
| Log event under one span formatted and submitted | 1,934 ns (1,802–2,415) | 1,163 ns (852–3,418) |
| Log events per second from eight threads | 1.04 M (0.88–1.09 M) | 2.07 M (0.88–2.43 M) |
| Log events per second from one thread | 0.89 M (0.73–1.04 M) | 0.94 M (0.29–1.18 M) |
| Job payload scanned for a decoded NUL, 64 KiB without escapes | 29.9 µs | 5.6 µs |
| Job payload scanned, 4 KiB with an escape every 8 bytes | 3.3 µs | 0.37 µs |
| Webhook signature of a 1 KiB body | 1,338–1,364 ns | 1,177–1,214 ns |
| Event prepared and its subject handed to the client | 5,860 instructions | 4,940 instructions |

The log rows overlap at the edges of their ranges; the earlier run of the
eight-thread case did not (0.75–1.10 M against 1.29–2.76 M). The webhook row
gives the medians of three passes, in which the new code won 29 of 36 paired
rounds. The event row counts instructions retired, which load does not move.
The first verification of a bearer JWT now also stores it; that cost could not
be separated from the noise (best round 9.8 µs before against 11.6 µs after
for RS256, 33.5 µs against 33.2 µs for ES256).

Measured and not adopted. Comparing a webhook signature as encoded text
instead of decoded bytes: no gain (1,463–1,579 ns against 1,507–1,511 ns).
Writing an error body once instead of twice: 2,649 → 1,481 ns for the two
functions, but it needed the rendered body cached inside `Problem`, cleared by
every setter, and the request id spliced into the JSON by byte position, for a
gain on error responses only. `<[u8]>::contains` as the only early exit of the
NUL scan: 10–20% slower on payloads that contain escapes. `sleep_until` in
`Deadline::wait`: a zero budget stayed pending on its first poll.

### Verified bearer JWT memo

`JwtVerifier` in `infra-bearerauthn` remembers a bearer JWT only after its
full verification succeeded, keyed by the token bytes. An entry holds the
validated principal, the token's `nbf`, and the generation of the key set that
verified the signature. Every parsed key set gets a process-unique generation;
a hit counts only while the installed set has the entry's generation.
Otherwise the entry is dropped and the token takes the full path, including
the refresh for an unknown `kid`, so a key that left the JWKS stops admitting
its tokens exactly as before. A refresh that fetches identical keys costs one
full verification per remembered token. On every hit `exp`, `nbf` and the
leeway are checked by the full path's own function and clock, and the caller
records the same metrics and events as for a full verification. A refusal is
never remembered. Bounds: 1024 entries, and per entry the shorter of the
token's remaining time to `exp` and one JWKS refresh period of 15 minutes.
The principal already kept the presented token, so the memo adds up to 1024
more live tokens and no new kind of retention.

Reopen if most tokens are seen once, so that the insert on a miss outweighs
the hits, or if anything besides the token, the installed key set and the
clock starts to influence a verification.

### Global allocator

Measured on 2026-09-29 on a DigitalOcean c-4 in London (Xeon 8280, Ubuntu
24.04, Rust 1.98.1, fat LTO, aligned code layout). The release service ran on
two CPUs, with PostgreSQL 16 on a third CPU and wrk on the fourth, using 64
connections against the Rust/Go comparison's contract fixtures. The matrix
figures are medians of five interleaved rounds of 20 seconds each, with a fresh
process for every cell. The soak test ran 30 minutes of mixed traffic: 1 KiB
and 64 KiB JSON, reads, and writes.

| Workload | CPU per request, glibc | mimalloc 0.1.52 | jemalloc 0.7.0 |
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

Plaintext responses were bimodal between rounds for every allocator, so they
do not decide anything. Resident memory stayed flat during the 30 minutes for
all three allocators. jemalloc matches mimalloc's CPU gain at less than half of
its extra memory, and it returns memory when idle.

The jobs engine was measured the same way with the `infra-jobs` bench on a
second c-4 client against PostgreSQL 18 on its own c-4: five interleaved
rounds of draining 20,000–40,000 no-op jobs, then a 20-minute soak at 1,000
jobs per second with 64 workers. Throughput stays within ±3% because
PostgreSQL sets the ceiling.

| Jobs worker | glibc | mimalloc | jemalloc |
| --- | ---: | ---: | ---: |
| CPU per job, 16 workers, 64 B payload | 50.0 µs | −8% | −5% |
| CPU per job, 64 workers, 64 B payload | 25.8 µs | −9% | −11% |
| CPU per job, 16 workers, 4 KiB payload | 58.5 µs | −11% | −3% |
| CPU per job, 64 workers, 4 KiB payload | 34.0 µs | −9% | −4% |
| Peak resident memory, drain | 10.5 MB | 26 MB | 13 MB |
| CPU over the 20-minute soak | 219 s | −7% | −3% |
| Resident memory after the soak | 82.2 MB | 98.5 MB | 79.5 MB |

Most of the soak's resident memory is the bench's own latency samples, and
they are the same for every allocator. `jobs-worker` uses jemalloc for the same
reason as the service and so that both long-running binaries share one
allocator.

### Open-file limit

Docker since Engine 25, containerd since 1.8 and systemd since 240 start a
process with a soft limit of 1024 open files under a hard limit of 524288. The
soft value stays low for programs that call `select(2)`, which cannot handle a
descriptor above 1023, and systemd's guidance is that other programs raise
their own soft limit to the hard one. The Go runtime does so unconditionally
since 1.19, and Deno does the same. Without it, each socket counts against
1024, so `http.max_connections` (4096 by default) is unreachable and accepts
past about a thousand connections fail with `EMFILE`.

`rlimit::increase_nofile_limit(u64::MAX)` is the established helper. It sets
the soft limit to the hard one, leaves the hard limit alone, and on macOS caps
the value at `kern.maxfilesperproc`, which `setrlimit` would otherwise reject.
`rlimit` 0.11.0 (MIT, released 2026-02-01) was already in the build through
`metrics-process`, which reads the same limit for `process_max_fds`, so the
binaries add no crate. `nix` exposes `setrlimit` too but leaves the macOS
ceiling and an unlimited hard value to the caller. Like Go, the binaries do
not lower the hard limit, read a configured value, or fail startup; Go also
restores the old soft limit for child processes, and these binaries start
none. The platform owns the hard limit (`--ulimit nofile`, `LimitNOFILE`).

## First request DTO or constrained parameter

Choose one validation library, not two. Start by evaluating
[`validator`](https://docs.rs/validator/latest/validator/) for ordinary REST
DTOs; prefer [`garde`](https://docs.rs/garde/latest/garde/) when enum and nested
container validation, explicit skipped fields or validation context make the
actual model simpler. Keep cross-field configuration budgets and domain
invariants with their existing owners rather than replacing them with opaque
custom attributes.

Before using a ready-made extractor such as
[`axum-valid`](https://docs.rs/axum-valid/latest/axum_valid/), inspect its exact
Axum and validator dependency versions. The researched `axum-valid` 0.25 line
uses `validator` 0.20, not 0.21, while its `garde` dependency is 0.23; do not
assume independently selecting the latest versions creates compatible traits.
A small transport-owned extractor is appropriate when the ready-made one
cannot preserve the service's error policy without a larger wrapper.

The implementation must preserve all of these contracts:

- Map extraction and validation failures to the existing `Problem` catalog,
  content type, status policy, request ID and `invalid_params` representation;
  JSON-body paths use RFC 6901 pointers, query/path parameters retain their
  documented locations. Do not turn body-limit failures into validation errors.
- Construct client-visible reasons from an allow-listed constraint description.
  Never serialize the validation library's error object or submitted values:
  `validator` can include the submitted value in its error parameters. Escape
  `~` and `/` when producing JSON-pointer segments.
- Keep `serde`, runtime constraints and `utoipa` schemas consistent. Deriving
  validation does not by itself prove OpenAPI constraints agree. Test the
  accepted boundary and rejected boundary, nested paths, unknown fields and
  secret-free error output for the new operation.

The first operation owns production integration. The executable HTTP recipe
exercises `validator` as a dev-dependency; it does not publish a generic
validator API, placeholder endpoint or authentication mechanism.

## Serialization, value types and construction

| Trigger | Preferred candidate | What must remain explicit |
| --- | --- | --- |
| Repeated custom serializers or a nonstandard external representation | [`serde_with`](https://docs.rs/serde_with/latest/serde_with/) adapters | The wire contract, unknown-field policy and OpenAPI shape. Do not replace working `humantime-serde` or `bytesize` merely for uniformity. It is now used for omission of optional `Problem` fields (`Code` serializes through its own `as_str`); the recipe suite also exercises nested adapters and PATCH states. |
| PATCH distinguishes missing, null and a value | `serde_with::rust::double_option` with the documented `default` and omission attributes | Missing means unchanged, null means clear, and a value means set. Test all three states and schema agreement; do not collapse them into `Option<T>`. |
| Repeated mechanical standard-trait implementations on newtypes | [`derive_more`](https://docs.rs/derive_more/latest/derive_more/), only the used derives | Preserve checked `TryFrom` construction. Do not generate `From<String>`, mutable dereferencing or setters that bypass a type's invariants. Keep `thiserror` for typed errors. |
| A genuinely complex constructor with required and optional inputs | Evaluate [`bon`](https://docs.rs/bon/latest/bon/) first | Preserve construction invariants and the actual API. Simple struct literals and `Problem::new(code)` remain appropriate. Never generate independent setters for status/title/URI derived from `code`. |
| A struct-only builder needs an alternative | [`typed-builder`](https://docs.rs/typed-builder/latest/typed_builder/) | Compare compile-time required-field checks and diagnostics against `bon` on the actual API; do not install both. |
| An existing API intentionally validates builder completeness at runtime | [`derive_builder`](https://docs.rs/derive_builder/latest/derive_builder/) | A fallible `build()` is a different contract, not an interchangeable type-state builder. Keep only one builder mechanism. |
| A compound collection operation replaces substantial manual plumbing | [`itertools`](https://docs.rs/itertools/latest/itertools/) | Prefer standard iterators and clear loops when equally direct. Do not replace a readable loop with a harder-to-follow combinator chain. |

## First persistence repository

Keep SQLx as the default. Use
[`FromRow`](https://docs.rs/sqlx/latest/sqlx/trait.FromRow.html) to remove
mechanical `Row::try_get` mapping when appropriate. `FromRow` alone does not
check SQL against the database at compile time. For suitable static queries,
use [`query!` and related macros](https://docs.rs/sqlx/latest/sqlx/macro.query.html)
with committed offline `.sqlx` metadata and the existing planned prepare check.
This is already a deferred decision, not a second migration framework or a query invented to justify tooling. The
trigger is the first *feature-owned* repository: an optional profile's own
provider-owned statements over its own table do not by themselves adopt
`query!`.

Consider [`SeaORM`](https://www.sea-ql.org/SeaORM/) only for an actual
CRUD-heavy derived service where entity/relation machinery removes enough
repetition. Compare concrete SQLx and ORM implementations first. Reconcile its
connection and transaction APIs with DSN admission, the existing migrations
and `CommitUnknown`; do not introduce a second schema authority or silently
bypass the transaction seam. Verify the selected release's SQLx compatibility
rather than assuming any SeaORM release can share the current pool.

## Outbound integrations and stateful capabilities

| First real need | Candidate | Required boundary |
| --- | --- | --- |
| Outbound HTTP adapter | [`reqwest`](https://docs.rs/reqwest/latest/reqwest/) | One reusable configured client, explicit request budgets, TLS/provider compatibility and safe errors. Inspect its native retry support before adding retry middleware. |
| Repeated eligible retry mechanics | [`backon`](https://docs.rs/backon/latest/backon/) | The operation owns idempotency, eligible errors, jitter/backoff, total deadline and cancellation. Never blindly retry `CommitUnknown`, non-idempotent effects, or stack independent retry loops. |
| Bounded process-local cache | [`moka`](https://docs.rs/moka/latest/moka/) | Specify capacity, expiration, invalidation and source of truth. It is not a distributed cache and does not make the existing readiness snapshot obsolete. |
<!-- template:begin cache:docs-library-selection-cache -->
| Distributed cache across replicas | `redis` 1.8.1 in `infra-cache`, default features off, only `tokio-comp` and `tokio-rustls-comp` | One owned supervisor over canonical multiplexed connections; no pool. Reuses `backon` and `tokio-util` for bounded recovery/cancellation. Composition or service adapters call the provider; features own keys, serialization, TTL, invalidation, and fallback. Independent Moka copies remain valid across multiple replicas when feature consistency permits. |
<!-- template:end cache:docs-library-selection-cache -->
<!-- template:begin object-storage:docs-library-selection-object-storage -->
| S3-compatible object storage | `aws-sdk-s3` 1.151.0 in `infra-object-storage`, default features off, only `rt-tokio` and `default-https-client`, with `aws-smithy-http-client` (`rustls-aws-lc`) building the HTTPS client | One client per bucket, pinned `BehaviorVersion`, no multipart. `aws-config` 1.12.0 (default features off) supplies only the workload identity providers for Amazon S3, never the default chain. The feature owns keys and retention. |
<!-- template:end object-storage:docs-library-selection-object-storage -->
<!-- template:begin service-auth:docs-library-selection-service-auth -->
| Caller-signed service tokens | `jsonwebtoken` 11.1.0 (`aws_lc_rs`, `use_pem`) for header and key types and for signing, `aws-lc-rs` for ES256 verification, in `infra-serviceauth` | One header and one payload parse per token, a parsed public key per admitted registry key, and no second crypto family. See [Service authentication decisions](decisions/service-authentication-decisions.md). |
<!-- template:end service-auth:docs-library-selection-service-auth -->
| Tests of an outbound HTTP contract | [`wiremock`](https://docs.rs/wiremock/latest/wiremock/) | Local mock server, bounded waits and actual status/body/header behavior. Recheck maintenance at adoption; do not use it to replace inbound-router tests. |
| A complex stable output warrants a reviewed snapshot | [`insta`](https://docs.rs/insta/latest/insta/) | Assert important semantics separately. Redact only irrelevant nondeterminism; never hide the ID or timestamp relationship being tested. Do not create a second snapshot authority for the already committed OpenAPI document. |
| A parser/transformation has useful algebraic or grammar invariants | [`proptest`](https://docs.rs/proptest/latest/proptest/) | State the property independently of the implementation, retain useful explicit boundary cases, and bound generation. It is not mandatory for every helper. |
<!-- template:begin jobs:docs-library-selection-jobs -->
| Durable background jobs on PostgreSQL | The template-owned engine in `crates/infra-jobs` | Enqueue inside the caller's transaction; kinds and handlers in adapter crates; effects idempotent; reassess the maintained queue crates before replacing the engine |
<!-- template:end jobs:docs-library-selection-jobs -->
<!-- template:begin messaging:docs-library-selection-messaging -->
| Typed durable JetStream transport interoperable with Go | `async-nats = "=0.50.0"`, default features off, with only `jetstream`, `aws-lc-rs`, and `nkeys` in `infra-messaging` | Reuse `time`, `serde`/`serde_json`, `bytes`, `sha2`, `base64`, `secrecy` (the credentials stay a `SecretString` until the client reads them), Tokio, metrics, and tracing; `opentelemetry` and `tracing-opentelemetry` carry the W3C trace context through message headers with the installed propagator, as `infra-jobs` does for its carrier. AWS-LC is the workspace Rustls provider; do not also enable `ring`, whose coexistence makes native client-provider selection ambiguous. Keep normal hostname verification and native roots. No chrono, websocket, KV, object-store, service, nuid, crypto, experimental, or server-extension feature is admitted. `foldhash` 0.2 (default features off, `std`) hashes the event registry maps; hashbrown already depends on it, and the keys are compile-time constants no caller can choose. `utoipa` (default features off) supplies the payload schemas of the AsyncAPI contract document (`Registry::asyncapi`); it is the schema derive the REST contract already uses. `nkeys` 0.4.5 (default features off; the latest release, 2025-07-03) is the key type `async-nats` already resolves for its `nkeys` feature. The adapter names it directly to sign a connection challenge for `messaging.credentials_file`: the client's own `credentials_file` reads the file once, and its per-connection `with_auth_callback` hands the signing to the caller. No crate reads the credentials file layout apart from a client; the adapter's twenty-line reader is checked against the client's parser in its tests. |
| Provider-free typed event identity | `domain-events`, a small local contract crate | It exists to enforce feature-to-provider dependency direction. It does not mint IDs, read clocks, own routes, or introduce a generic producer/event-bus API. |
<!-- template:end messaging:docs-library-selection-messaging -->

The recipe suite exercises the selected utility mechanisms without installing
a cache, HTTP provider, retry policy or new endpoint in the running service.
The owning feature/profile still supplies production policy and wiring.

## Retained mechanisms and rejected blanket changes

Keep `axum`, `tower`/`tower-http`, `serde`, `config`, `clap`, `thiserror`,
`utoipa`/`utoipa-axum`, SQLx, and the current telemetry and lifecycle stack.
Prefer their existing extension points before adding substitutes. No broad
framework replacement, generic repository, dependency-injection container,
custom derive collection, mandatory ORM, cache, or retry engine is justified
by the health-only scaffold.

Do not add `strum` or a serde `snake_case` Serialize for `Code`: the
`service-failure` `codes!` macro already generates the variants, `Code::ALL`
and `as_str`, and a manual `Serialize` delegates to `as_str`, which keeps
`internal_error` distinct from the variant name.

The configuration loader retains source precedence and secret admission;
`Problem` retains the closed error policy and sanitized output; the HTTP
adapter retains framing, limits and fallback policy; transactions retain
commit-outcome classification. A shorter implementation that drops one of
these requirements is not a boilerplate reduction.

Keep Compose plus `#[sqlx::test]` for PostgreSQL tests. Do not add
`testcontainers` alongside the existing environment without a new requirement
that reopens the recorded persistence decision. Likewise, do not add a second
OpenAPI generator, snapshot, configuration loader or error library for a
mechanism the selected dependency already supplies.

<!-- template:begin messaging:docs-library-selection-messaging-rejection -->
Do not add a generic messaging framework, alternate NATS client, blocking
client, second worker executable, lifecycle crate, or stream-administration
wrapper. `async-nats` supplies the selected protocol mechanics; the local
adapter owns the accepted Go-compatible wire and settlement policy. Reconsider
that boundary only for an accepted second transport or changed feature
dependency direction. Do not add `asyncapi-rust` or `schemars` for the event
contract document (checked 2026-10-02): `asyncapi-rust` 0.5.0 declares
channels in derive attributes apart from the routes and takes payload schemas
from `schemars`, which would be a second schema derive beside `utoipa` with no
`time` integration. `cloudevents-sdk` 0.9.0 requires `async-nats ^0.42` and
cannot resolve beside the pinned client, one more reason against a
CloudEvents envelope.

The adapter enables `time/large-dates` because a Go-accepted RFC3339 timestamp
with a year-9999 offset can normalize to UTC year 10000. This keeps that
normalization representable without introducing a date parser or another
library. In time 0.3.55 the feature propagates only to `time-core` and an already
selected `time-macros`; it adds no package. The actual Go bridge separately
checks inbound acceptance, normalization, re-encoding, and decode of the emitted
timestamp: Go's own RFC3339 parser does not promise to round-trip every
`time.Time` value.
<!-- template:end messaging:docs-library-selection-messaging-rejection -->

<!-- template:begin cache:docs-library-selection-cache-rejection -->
Do not add `fred`, a Redis pool (`deadpool-redis` or `bb8-redis`), or
`testcontainers` for the cache profile. `fred` 10.1.0 was last released and
pushed on 2025-02-27. One multiplexed connection already pipelines get, set,
and delete. Container proof uses the existing Compose Valkey service. See
[Cache decisions](decisions/cache-decisions.md).
<!-- template:end cache:docs-library-selection-cache-rejection -->
<!-- template:begin object-storage:docs-library-selection-object-storage-rejection -->
Do not add `object_store`, `opendal`, `rust-s3`, `minio`, or
`aws-sdk-s3-transfer-manager` for object storage, and do not enable the SDK's
default features. `object_store` reports a create-only put retried after a 5xx
as already existing, `opendal` is a breaking 0.x with transport errors in one
kind, and the default SDK features bring hyper 0.14, rustls 0.21, and ring. See
[Object storage decisions](decisions/object-storage-decisions.md).
<!-- template:end object-storage:docs-library-selection-object-storage-rejection -->

## Acceptance for a library-driven refactor

Preserve existing assertions and runtime contracts; show which manual code or
synchronization point disappeared. Inspect the resolved feature graph and
duplicate versions, keep test support under dev-dependencies, and run the
build and workspace tests when manifests or the lockfile change. Keep existing
CI, advisory, license and unused-dependency gates intact. Report the actual
validation and any unavailable evidence; do not claim a percentage reduction
or performance gain without a measured comparison.

## Telemetry compatibility

The telemetry crate's JSON layer reads `trace_id`, `span_id`, and
`trace_flags` through `tracing_opentelemetry::get_otel_context` with
the active dispatch, so the log line does not depend on a logging crate's
bridge to a particular `tracing-opentelemetry` minor.


### Bounded stdout writer

The adopted logging composition uses the standard library's bounded channel
and one owned output thread in `infra-telemetry::logging::output`, with PR #245's
shared bounded JSON/text capture and private diagnostic boundary. The PR #244
library comparison remains relevant to the queue and shutdown mechanism; its
separate logger implementation and completion policy are superseded.
`crates/infra-telemetry` owns the fixed byte, record, callback and span
limits, loss accounting and final-result policy; the composition root owns
guard custody, the shared telemetry deadline and process exits.

The source comparison used Rust 1.99.0, Tokio 1.53.1 and
tracing-subscriber 0.3.23 on 2026-10-05 and added no dependency:

| Candidate inspected | Deciding limitation or fit |
| --- | --- |
| Synchronous stdout at the event site | A stalled sink blocks the emitting runtime worker. |
| `tracing-appender` 0.2.5 | A finite lossy queue supplies admission, but its error counter observes failed sends rather than sink failures. Its worker discards I/O errors and acknowledges before final flush; guard Drop has fixed waits, no typed terminal result and possible fallback output. A sink wrapper cannot repair that completion/destructor contract. |
| `flexi_logger` 0.31.10 | Async mode uses an unbounded channel; `pool_capa` limits reusable buffers rather than queued records. Its shutdown/flush APIs do not provide the caller-owned deadline and drop-newest contract. |
| Tokio 1.53.1 bounded mpsc plus a thread | Viable, but still requires admission close, final status and completion observation outside the runtime. The standard channel already supplies this synchronous boundary. |
| Standard bounded channel plus one thread | `try_send` distinguishes full and disconnected queues; sender closure ends admission without requiring queue capacity, and bounded completion waiting lets the root retain its deadline. Thread-handle drop detaches. |

The local mechanism supplies nonblocking admission, finite retained storage,
observable write/flush failure and completion, and a nonblocking destructor
without fallback output. It adds no process policy or second logger. A timed-out
wait leaves actual sink custody with the bounded writer; it does not establish
that the writer stopped.

Replace the local mechanism when a maintained component's supported API meets
these capture, privacy, accounting and lifetime constraints with less overall
code. Reopen the comparison if an observed workload makes the fixed capacity,
copying or thread cost unsuitable.

Source comparison references: [tracing-appender 0.2.5](https://docs.rs/tracing-appender/0.2.5/src/tracing_appender/non_blocking.rs.html),
[flexi_logger 0.31.10 write modes](https://docs.rs/flexi_logger/0.31.10/flexi_logger/enum.WriteMode.html),
[standard try_send](https://doc.rust-lang.org/std/sync/mpsc/struct.SyncSender.html#method.try_send),
and [thread handle drop](https://doc.rust-lang.org/std/thread/struct.JoinHandle.html).
