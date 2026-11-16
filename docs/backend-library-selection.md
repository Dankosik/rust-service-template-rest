# Backend library selection

Use this guide when a feature needs a new dependency or starts repeating a
mechanical implementation. `Cargo.toml`, `Cargo.lock`, and `crates/` own
declared versions, enabled features, and active call sites; this document
records only benchmark evidence, non-code rejection reasons, and selection
criteria for capabilities not yet adopted (`docs/decisions/` owns adapter-level
choices).

A dependency earns its place by deleting a current mechanism or a manually
maintained relationship. Prefer the standard library and existing workspace
dependencies first. Declare versions once in `[workspace.dependencies]` with
`default-features = false`, enable only needed features in consuming crates, and
recheck maintenance, licensing, and advisories at adoption time.

## Rejected candidates

- **HTTP / header / MIME helpers:** `mime` in `shared-rules` (violates `std`-only charter and rejects parameters RFC 9110 provider checks accept); `headers` for `Bearer` / `Retry-After` (looser than RFC 6750 and lacks `Retry-After` value parsing).
- **Concurrency, clocks, and collections:** `quanta`, `recloser`, and `failsafe` (clocks ignore `tokio::time::pause`); `arc-swap` on JWKS refresh (already serializes off the hot path behind `moka` memoization); `indexmap` on tiny static registries (`Vec` `8.3 ns` vs `IndexMap` `18.0 ns` at 8 entries, `12.2 ns` vs `18.5 ns` at 16 entries); `atoi` (`7.0 ns` vs `7.6 ns` against `str::parse` on validated ASCII digits) and `itoa` (`std::fmt::NumBuffer` formats integers on the stack in `std`); `smol_str`, `dashmap`, `papaya`, and `phf` (covered by workspace string, map, and enum primitives without duplicate abstractions); `wait-timeout` (installs a process-wide `SIGCHLD` handler alongside `tokio::process`).
- **Serialization and config:** `sonic-rs` and `simd-json` (replace the JSON contract rather than one mechanism); `serde_with::OneOrMany` / `PickFirst` in config (error messages can echo secret/raw values); `deranged` in config (turns structured validation errors into decode errors); `strum` on failure/metric enums (wire/metric spellings intentionally differ from variant names).
- **Logging and testing:** `tracing-appender` (drops sink I/O errors, acknowledges before final flush, and blocks in `Drop`); `flexi_logger` (unbounded async channel and no caller-deadline drop-newest contract); `std::sync::mpsc::sync_channel` for idle background writers (lacks multi-channel selection, forcing `recv_timeout` wakeups vs `0` wakeups/s on `crossbeam-channel`); `tracing-test` (global rendered-text matcher cannot isolate concurrent span/field assertions); `tracing-capture` and `prometheus-parse` (unmaintained upstream); `openmetrics-parser` (LGPL-3.0); `tokio-test` (`futures_util::poll!` suffices).
<!-- template:begin authn:docs-library-selection-authn-rejection -->
- **Inbound bearer authentication:** `jwt-authorizer` 0.15 (last release 2024-08, on `jsonwebtoken` 9), `tower-oauth2-resource-server`, and `axum-jwt-auth` (none documents a rate-limited unknown-`kid` refresh or per-entry JWKS admission); `infra-outbound-http` for discovery, JWKS, and introspection calls (`AUTHN` is selectable without `OUTBOUND_HTTP=bounded`, so the authenticator would force that profile; reopen when every authentication profile selects it); `moka` `try_get_with` for introspection single-flight (a dropped initiating caller restarts the provider request inside another waiter's future); `CancellationToken::run_until_cancelled` in the JWKS refresh worker (it favours a finished fetch over cancellation, where shutdown must win).
<!-- template:end authn:docs-library-selection-authn-rejection -->
<!-- template:begin messaging:docs-library-selection-messaging-rejection -->
- **Messaging schemas and envelopes:** `asyncapi-rust` and `schemars` (introduce a second schema derive alongside `utoipa` without `time` parity); `cloudevents-sdk` (conflicts with the pinned `async-nats` line). Keep `ring` off so the workspace shares one Rustls provider (`aws-lc-rs`), and keep `time/large-dates` enabled so Go-accepted RFC 3339 year-9999 offsets normalize to UTC year 10000 without a custom parser.
<!-- template:end messaging:docs-library-selection-messaging-rejection -->

## Recorded benchmarks and runtime constraints

### Hot-path primitives

Measured on a dedicated 4-vCPU Xeon 8280 DigitalOcean c-4 (`linux/amd64`, Debian 13, `rustc 1.100.0`, fat LTO, `codegen-units = 1`, `jemalloc`, 15 interleaved rounds):
- Contended 4-thread read-mostly lookups (`std::sync::RwLock<HashMap>` → `arc-swap` + `foldhash`): `314 ns` → `44.4 ns` (`694–789 ns` → `241–319 ns` on shared handle reads under per-request work).
- Closed-set counters/gauges (`RwLock<HashMap>` → `enum-map`): `28.0 ns` → `2.04 ns`; inline `≤ 24` B strings (`compact_str`): `7.35–72.1 ns` → `2.20–30.2 ns`; stack scratch slices (`smallvec`): `51.2–79.6 ns` → `35.4–37.4 ns`; non-poisoning Linux `futex` locks (`parking_lot`): `14.2 ns` → `12.5 ns`.
- End-to-end core-pinned Axum HTTP (`2 vCPU` server / `2 vCPU` client, 50% `200` JSON + 50% `400 Problem`): `97,630` → `119,030 req/s` (`+21.9%`), `20.11` → `16.72 µs` CPU/req (`−16.9%`), `p99` `664` → `424 µs` (`−36.1%`) at `21.2 MB` RSS.

<!-- template:begin oidc-jwt:docs-library-selection-bearer-jwt-memo -->
### Verified bearer JWT memo

Repeated Bearer JWT verification dropped from `21.7 µs` to `216 ns` (RS256) and `100.0 µs` to `248 ns` (ES256) with `moka` bounded by token `exp` and cleared on JWKS refresh. Reopen if tokens are predominantly single-use or verification depends on per-request inputs beyond token, key set, and clock.
<!-- template:end oidc-jwt:docs-library-selection-bearer-jwt-memo -->

### Global allocator

Measured on a 4-vCPU Xeon 8280 DigitalOcean c-4 (`linux/amd64`, fat LTO, 5 interleaved 20 s rounds plus a 30-minute mixed soak against PostgreSQL):

| Mixed 30-minute soak | glibc | mimalloc | jemalloc |
| --- | ---: | ---: | ---: |
| Throughput | 9,027/s | +16% | +15% |
| p99 latency | 13.8 ms | 11.9 ms | 12.0 ms |
| Resident memory under load | 18.7 MB | 30.5 MB | 23.3 MB |
| Resident memory after 60 s idle | 18.1 MB | 28.0 MB | 17.6 MB |

Long-running binaries select `jemalloc` (`tikv-jemallocator`) because it matches `mimalloc`'s CPU reduction (−12% to −20% on JSON/PostgreSQL, −3% to −11% on background workers) while returning idle pages to the OS (`79.5 MB` vs `98.5 MB` post-soak worker RSS).

### Open-file limit

Docker (since Engine 25), containerd (since 1.8), and systemd (since 240) start containers with a soft `RLIMIT_NOFILE` of `1,024` under a higher hard limit for legacy `select(2)` compatibility; raising soft to hard at startup via `rlimit` prevents `EMFILE` accept failures around 1,000 connections while leaving the hard ceiling platform-owned.

## Unadopted candidates for future features

| Need | Candidate | Boundary to preserve |
| --- | --- | --- |
| Request DTO validation | Choose one per service: `validator` (flat DTOs) or `garde` (nested/contextual rules) | Map to `Problem` (`invalid_params` + RFC 6901 pointers) from allow-listed descriptions; **never echo raw crate error structs or submitted values** (`validator` embeds raw inputs in parameters). Keep `serde`, validation, and `utoipa` schemas aligned. |
| `PATCH` tri-state (`missing` vs `null` vs value) or custom wire formats | `serde_with` (`double_option`) | Preserve unknown-field rejection and OpenAPI schema parity. |
| Complex constructors with optional inputs | Install at most one: `bon` or `typed-builder` (`derive_builder` only for runtime-fallible `build()`) | Never expose setters that bypass domain or `Problem` invariants. |
| CRUD-heavy persistence beyond SQLx `query!` | Re-evaluate `SeaORM` only on demonstrated CRUD dominance | Preserve single migration ownership, DSN admission, and `CommitUnknown` handling. |
| Outbound retry loops | `backon` | Caller owns idempotency, retryable error class, jitter, and outer deadline; never retry `CommitUnknown` or stack retry loops. |
| Contract mocks, snapshots, or property tests | `wiremock`, `insta`, `proptest` (dev-only) | Never replace inbound router tests or duplicate committed OpenAPI snapshots. |
