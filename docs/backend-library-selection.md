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
- **Concurrency, clocks, and collections:** `quanta`, `recloser`, and `failsafe` (clocks ignore `tokio::time::pause`); `indexmap` on tiny static registries (`Vec` `8.3 ns` vs `IndexMap` `18.0 ns` at 8 entries, `12.2 ns` vs `18.5 ns` at 16 entries); `atoi` (`7.0 ns` vs `7.6 ns` against `str::parse` on validated ASCII digits) and `itoa` (`std::fmt::NumBuffer` formats integers on the stack in `std`); `smol_str`, `dashmap`, `papaya`, and `phf` (covered by workspace string, map, and enum primitives without duplicate abstractions); `wait-timeout` (installs a process-wide `SIGCHLD` handler alongside `tokio::process`).
- **Serialization and config:** `sonic-rs` and `simd-json` (replace the JSON contract rather than one mechanism); `serde_with::OneOrMany` / `PickFirst` in config (error messages can echo secret/raw values); `deranged` in config (turns structured validation errors into decode errors); `strum` on failure/metric enums (wire/metric spellings intentionally differ from variant names).
- **Logging and testing:** `tracing-appender` (drops sink I/O errors, acknowledges before final flush, and blocks in `Drop`); `flexi_logger` (unbounded async channel and no caller-deadline drop-newest contract); `std::sync::mpsc::sync_channel` for idle background writers (lacks multi-channel selection, forcing `recv_timeout` wakeups vs `0` wakeups/s on `crossbeam-channel`); `tracing-test` (global rendered-text matcher cannot isolate concurrent span/field assertions); `tracing-capture` and `prometheus-parse` (unmaintained upstream); `openmetrics-parser` (LGPL-3.0); `tokio-test` (`futures_util::poll!` suffices).
<!-- template:begin authn:docs-library-selection-authn-rejection -->
- **Inbound bearer authentication:** `jwt-authorizer` 0.15 (last release 2024-08, on `jsonwebtoken` 9), `tower-oauth2-resource-server`, and `axum-jwt-auth` (none documents a rate-limited unknown-`kid` refresh or per-entry JWKS admission); `infra-outbound-http` for discovery, JWKS, and introspection calls (`AUTHN` is selectable without `OUTBOUND_HTTP=bounded`, so the authenticator would force that profile; its `Client` admits one origin while discovery can name a `jwks_uri` on another host, and its only downstream test constructor is plain-HTTP loopback, which drops the HTTPS-only, DNS, and TLS fixture proofs; `reqwest` stays linked through the OTLP exporter either way; reopen when every authentication profile selects it and it offers an HTTPS fixture constructor); `moka` `try_get_with` for introspection single-flight (a dropped initiating caller restarts the provider request inside another waiter's future, and moka 0.12.16 panics a waiter after 200 aborted initializers); `CancellationToken::run_until_cancelled` in the JWKS refresh worker (it favours a finished fetch over cancellation, where shutdown must win); rewriting verified claims so the last repeated member wins before `Principal::claims` decodes them (it took an own JSON walker and `indexmap` to keep a rule RFC 7519 §4 leaves optional; `serde_json` decodes the verified payload directly, so a type that reads a repeated member is refused, and 128-bit integers stay exact).
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

Repeated Bearer JWT verification dropped from `21.7 µs` to `216 ns` (RS256) and `100.0 µs` to `248 ns` (ES256) with `moka` bounded by token `exp` and cleared on JWKS refresh, including a refresh that returns the same document: that costs one full verification per live token per refresh, and refreshes on demand are at least 30 s apart, while comparing documents would add state to guard the same rule. Reopen if tokens are predominantly single-use, verification depends on per-request inputs beyond token, key set, and clock, or re-verification after a refresh shows up in a profile.
<!-- template:end oidc-jwt:docs-library-selection-bearer-jwt-memo -->

### Runtime workers

Measured on a 4-vCPU Xeon 8168 DigitalOcean c-4 (Debian 13, wrk with 64 connections on two other CPUs, median of three interleaved 15 s rounds). Workers equal to the CPUs the process may use is the choice.

- **More workers than CPUs trades tail latency for throughput.** On two CPUs, 24 workers against two served a trivial route at 33.4 against 28.9 thousand req/s and 64 KiB JSON at 3,748 against 3,480 req/s, because threads stop parking (0.04 against 0.50 voluntary context switches per request); p99 rose from 4.0 to 33.7 ms and from 29.9 to 91.1 ms, and resident memory from 33 to 52 MB and from 47 to 123 MB.
- **A current-thread runtime on one CPU gains nothing** over the multi-thread runtime with one worker: 16.8 against 17.4 thousand req/s on the trivial route, 2,306 against 2,265 on 64 KiB JSON, 1.2 MB less memory.
- **A fractional CPU quota needs an explicit count.** `std::thread::available_parallelism` rounds a quota down. Under a 1.5-CPU quota one worker served 17.1 of 19 thousand offered req/s at a 3.9 ms median; two workers served 18.9 thousand at 0.75 ms with no throttled period, and 23.1 of 24 thousand offered with p99 4.4 ms and 3.5 throttled periods in 15 s.

### Global allocator

Measured on a 4-vCPU Xeon 8280 DigitalOcean c-4 (`linux/amd64`, fat LTO, 5 interleaved 20 s rounds plus a 30-minute mixed soak against PostgreSQL):

| Mixed 30-minute soak | glibc | mimalloc | jemalloc |
| --- | ---: | ---: | ---: |
| Throughput | 9,027/s | +16% | +15% |
| p99 latency | 13.8 ms | 11.9 ms | 12.0 ms |
| Resident memory under load | 18.7 MB | 30.5 MB | 23.3 MB |
| Resident memory after 60 s idle | 18.1 MB | 28.0 MB | 17.6 MB |

Long-running binaries select `jemalloc` (`tikv-jemallocator`) because it matches `mimalloc`'s CPU reduction (−12% to −20% on JSON/PostgreSQL, −3% to −11% on background workers) while returning idle pages to the OS (`79.5 MB` vs `98.5 MB` post-soak worker RSS).

jemalloc purges freed pages only from the thread that allocates in the same arena unless its background threads run, so Linux glibc builds enable `background_threads`, and the runtime image bakes `thp:never`. Measured on a 4-vCPU Xeon 8168 DigitalOcean c-4 (Debian 13, service on two CPUs with two workers, a 20 s burst of 64 KiB JSON over 256 connections, then 90 s idle):

| Resident memory | Host THP `madvise`, no background threads | `madvise`, background threads | Host THP `always`, no background threads | `always`, background threads | `always`, background threads + `thp:never` |
| --- | ---: | ---: | ---: | ---: | ---: |
| At start | 13.9 MB | 13.8 MB | 30.6 MB | 30.3 MB | 23.4 MB |
| Burst peak | 35.3 MB | 39.3 MB | 64.2 MB | 50.4 MB | 52.3 MB |
| After 90 s idle | 31.1 MB | 15.1 MB | 65.2 MB | 48.3 MB | 25.8 MB |

CPU per request stayed within ±1% for both options (plain and 64 KiB JSON, five interleaved 15 s rounds); resident memory under sustained 64 KiB load rose by about 6 MB because application threads no longer purge inline. With `always`, khugepaged collapses the sparse regions jemalloc purged back into huge pages, outside the allocator's own accounting. `_RJEM_MALLOC_CONF` overrides the baked options at process start. The `stats` feature (needed by `tikv-jemalloc-ctl` gauges) cost 6% CPU per request on the 64 KiB path and stays off; reopen it when allocator-level memory attribution is needed.

### Listener sockets

Measured on a 4-vCPU Xeon 8168 DigitalOcean c-4 (Debian 13, kernel 6.12, `net.core.somaxconn` 4096, service on two CPUs with two workers, loopback):

- **Listen backlog requested at 65,535 instead of `TcpListener::bind`'s 128.** mio passes a fixed 128, so a burst of new connections overflows the accept queue and each dropped handshake retries after one second. The kernel caps the request at `net.core.somaxconn`.

| Simultaneous connections | Backlog 128 | Backlog 4096 |
| --- | --- | --- |
| 300 | 118 wait ≥ 1 s, p99 1.04 s | p99 44–77 ms |
| 1,000 | 82% wait ≥ 1 s | p99 69–140 ms |
| 3,000 | p50 1.2–2.1 s, p99 2.3–3.1 s, 5–6 thousand dropped handshakes | p99 0.16–0.32 s, none dropped |

- **`TCP_USER_TIMEOUT` of 30 s on accepted connections**, the value the outbound client already uses. With the send buffer capped at 64 KiB, 200 clients that sent a request and stopped reading held 200 connections and 248 MB (a 0.9 MiB response each) for the whole 90 s observed; with a 10 s timeout they were gone within 20 s and memory returned to 43 MB. A client reading steadily at 32 KiB/s still received the full response. HTTP/2 connections were already covered by keep-alive PING.
- **`http.header_read_timeout` defaults to 75 s instead of 5 s.** hyper's HTTP/1 header timer starts when a connection begins waiting for its next request, so it is also the keep-alive idle timeout. A second request 4.8 s after the first succeeded and one after 5.2 s found the connection closed. Behind nginx with a 60 s upstream keep-alive and 1 ms of network delay, requests spaced about 5 s apart hit a closing connection 36 times in 2,560 and 9 of 1,280 `POST`s returned `502`; with 75 s there were none. A new connection per request cost 119.5 µs of CPU against 67.8 µs on a kept one. The cost is that a client sending its headers slowly holds a connection slot for up to 75 s; `http.max_connections` bounds the slots. Reopen if hyper gains a separate idle timeout.

### Open-file limit

Docker (since Engine 25), containerd (since 1.8), and systemd (since 240) start containers with a soft `RLIMIT_NOFILE` of `1,024` under a higher hard limit for legacy `select(2)` compatibility; raising soft to hard at startup via `rlimit` prevents `EMFILE` accept failures around 1,000 connections while leaving the hard ceiling platform-owned.

### Log writer

The log writer thread hands the sink one write of up to 64 KiB for the records that arrived within 2 ms of its previous write; records further apart are written as they arrive. One write and one wake of the writer per record ran it on the request workers' CPUs once per request. Measured on a dedicated 4-vCPU Xeon 8358 DigitalOcean c-4 (service on two CPUs, load on the other two, interleaved rounds):
- Closed loop, 64 connections, `/dev/null` sink: `49.4` → `41.7 µs` CPU/req (`−16%`), `39,963` → `47,866 req/s`, `p99` `2.89` → `2.04 ms`; with a 1 KiB JSON body `64.9` → `56.5 µs`. Into a pipe: `52.1` → `40.8 µs`, `37,914` → `48,892 req/s`.
- Open loop at 24,500 req/s: `1.26` → `1.09` cores, `p99.9` `3.46` → `3.04 ms`, `p99.99` `5.57` → `3.42 ms`, involuntary context switches `0.82` → `0.02` per request. At 2,000 req/s `74.3` → `65.3 µs` CPU/req; at 20 and 200 req/s unchanged (`167` and `86 µs`).
- Records lost to a full queue at saturation: up to `79` per 12 s run (`152` into a pipe) → `0`.

The gathering ends as soon as the queue holds a quarter of its 512 records, so a burst is written at once. Rejected: an unconditional 2 ms pause (a burst above the queue's capacity inside the pause is lost where the per-record writer kept up) and writing from the request thread (a blocked sink would block requests). The cost is that a record written under load reaches the sink up to 2 ms later; reopen if a sink needs it sooner.

### Overload admission and rollout

Measured on a 4-vCPU Xeon 8168 DigitalOcean c-4 (Debian 13, service on two CPUs with two workers, loopback):

- **`http.admission_wait` of 1 s instead of waiting the whole `http.request_timeout`.** The request budget starts at arrival, so a request admitted at the end of an 8 s wait has no time left. With 16 slots and a 500 ms handler (capacity 32 req/s) and an open-loop arrival of 64 req/s, the 8 s wait answered every request `504` (goodput 0); at 40 req/s successes took 5.8 s (p99 7.9 s). A 200 ms wait kept goodput at 31.8 req/s with successes in 0.69 s and `503` in 0.2 s; a 1 s wait kept 32.0 req/s with successes in 1.49 s. The default matches `grpc.admission_wait`. Reopen for a queue discipline (LIFO, CoDel) if a 1 s tail under overload is too long.
- **A standing queue for a PostgreSQL connection serves its newest caller first (`WaitOrder::NewestOnceStanding`, the service's pool).** Measured on a 4-vCPU Xeon 8358 c-4 with 4 connections, 20 ms statements, callers that give up after 1 s and retry twice, and a 15 s burst at twice the pool's capacity. In arrival order 6% of the burst's database requests were answered in time and none in the 60 s after it at 60% load: the wait for a connection settles at the callers' timeout, every answer is late, and the retries keep the queue full. Newest first once the queue has stood for 100 ms (the adaptive LIFO of "Fail at Scale") answered 55%, recovered within 2 s, and left an unrelated route at 100% of its requests; with callers that give up after 2 s the burst went from 17% to 58%. The driver's own queue is first come, first served, so only as many callers as the pool holds wait in it and the rest take turns in `infra-postgres`. The same order on the shared HTTP admission gave the same database numbers and cost the unrelated route 17% of its requests during the burst, so HTTP and gRPC admission stay in arrival order with `admission_wait`. The jobs worker's pool stays in arrival order: its callers wait as long as it takes, and a caller that has waited is still there. A background caller on the service's pool (the idempotency cleanup pass, 3 s per acquire) is served last while the queue stands, so a pass that meets a standing queue times out with `postgres_pool_acquire_timeout` and runs again on its schedule; the PostgreSQL readiness probe takes the next freed slot ahead of that standing queue so a replica still answering requests is not evicted. Reordering the waiters inside the vendored `sqlx-core` pool was rejected: its queue is the fair semaphore every acquire and connection return goes through, and the turns in front of it leave the vendored diff at the one bounded return it already carries. A shorter fixed wait and a CoDel-style patience were rejected: by Erlang C a 5 ms patience on a 4-connection pool refuses about 10% of requests at 50% utilisation, and those refusals can keep the short patience engaged. Reopen if a derived service needs a dependency's queue bounded as well as reordered (admission per dependency).
- **HTTP/1 responses carry `Connection: close` while readiness is draining.** hyper closes a keep-alive connection that is between requests as soon as the listener drains; a client that keeps sending through the readiness propagation delay (a Kubernetes Service, a direct caller) lost one request on each of 256 connections with a 50 µs handler and on 20–27 of 256 with a 10 ms handler. With the header every connection is retired by its own next response during the delay. A client that reopens a connection to the same replica at the instant the listener stops can still lose that request; gRPC lost none (`GOAWAY`).

## Unadopted candidates for future features

| Need | Candidate | Boundary to preserve |
| --- | --- | --- |
| Request DTO validation | Choose one per service: `validator` (flat DTOs) or `garde` (nested/contextual rules) | Map to `Problem` (`invalid_params` + RFC 6901 pointers) from allow-listed descriptions; **never echo raw crate error structs or submitted values** (`validator` embeds raw inputs in parameters). Keep `serde`, validation, and `utoipa` schemas aligned. |
| `PATCH` tri-state (`missing` vs `null` vs value) or custom wire formats | `serde_with` (`double_option`) | Preserve unknown-field rejection and OpenAPI schema parity. |
| Complex constructors with optional inputs | Install at most one: `bon` or `typed-builder` (`derive_builder` only for runtime-fallible `build()`) | Never expose setters that bypass domain or `Problem` invariants. |
| CRUD-heavy persistence beyond SQLx `query!` | Re-evaluate `SeaORM` only on demonstrated CRUD dominance | Preserve single migration ownership, DSN admission, and `CommitUnknown` handling. |
| Outbound retry loops | `backon` | Caller owns idempotency, retryable error class, jitter, and outer deadline; never retry `CommitUnknown` or stack retry loops. |
| Contract mocks, snapshots, or property tests | `wiremock`, `insta`, `proptest` (dev-only) | Never replace inbound router tests or duplicate committed OpenAPI snapshots. |
