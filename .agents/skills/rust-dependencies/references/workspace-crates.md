# Workspace utility crates

Consult this index before hand-rolling a non-domain helper, parser, encoder, retry loop, secret buffer, compact collection, or boilerplate trait impl. Declared versions live in `Cargo.toml` (in a derived service, re-add any pruned crate to `[workspace.dependencies]` with `default-features = false` when a feature adopts it); enable only needed features in the consuming crate. `crates/shared-rules` stays `std`-only.

## Boilerplate, derives, and compile-time helpers

- **Custom `Default` values or selective trait bounds** — `educe`: keep manual `Default` when a template identity anchor must remain literal (such as `observability.rs`).
- **Generic `Clone`/`Debug`/`Default`/`Eq` without bounds on `T`** — `derive-where`: avoids accidental `T: Debug` or `T: Default` bounds on generic containers and routes.
- **Redacted `Debug`, `Deref`, `Display`, `From`, `IntoIterator`** — `derive_more`: redact secrets (`#[debug("...")]`); reserve `Deref` for smart pointers.
- **Safe `Pin` projections for `Future`/`Stream`/`Body` adapters** — `pin-project-lite`: works under `unsafe_code = "forbid"`.
- **Typed error enums and `Display`/`#[from]` wiring** — `thiserror`: prefer over manual `Display` + `Error` boilerplate.
- **Compile-time constant string formatting** — `const_format`: `formatcp!` / `concatcp!` for static prefix or header constants.
- **Multi-option or staged builders** — `bon`: use only when plain structs or constructors obscure caller intent.
- **Async `Stream` construction** — `async-stream`: avoid hand-rolled poll state machines when a linear async generator suffices.

## Compact structures, concurrency, and caching

- **Short owned strings (`<= 24` B inline)** — `compact_str`: `CompactString` / `format_compact!`; keep `String` where a fixed public API requires it.
- **Fixed-capacity stack strings or byte buffers** — `arrayvec`: `ArrayString<N>` / `ArrayVec<T, N>` (enable `std` for `io::Write`) when the upper bound is static.
- **Usually-small inline vectors** — `smallvec`: `SmallVec<[T; N]>` for 1–8 common elements (enable `const_generics` for non-standard `N`); keep `N` small so struct size stays bounded.
- **Dense lookup or cached metric tables keyed by a closed enum** — `enum-map`: O(1) array indexing (`EnumMap<K, OnceLock<Counter>>`) without hashing.
- **Fast non-crypto maps, sets, or `moka` hashers** — `foldhash`: internal or bounded keys only; keep `std` SipHash for untrusted network keys.
- **Lock-free hot-read, rare-write snapshots** — `arc-swap`: `ArcSwap` / `ArcSwapOption`; never hold a `Guard` across `.await`.
- **Short-held non-poisoning locks** — `parking_lot`: compact `Mutex`/`RwLock`; never hold across `.await`.
- **Bounded concurrent caches with TTL and capacity eviction** — `moka`: avoid `try_get_with` for single-flight where dropped callers must not restart upstream work.
- **Zero-copy shared byte buffers across I/O frames** — `bytes`: `Bytes` / `BytesMut`.
- **Multi-channel blocking selection on dedicated threads** — `crossbeam-channel`: avoids timed polling wakeups that `std::sync::mpsc` forces.
- **Typed bitmasks** — `bitflags`: checked flag operations over raw integer masks.
- **Insertion-ordered maps or sets** — `indexmap`: skip on tiny static registries (`<= 16` items: linear `Vec` scan is faster).

## Parsing, scanning, encoding, and `serde`

- **MIME / `Content-Type` parsing** — `mediatype`: guard empty unquoted parameters explicitly (`!param.as_str().is_empty()`).
- **SIMD byte-slice search and non-UTF-8 byte formatting** — `bstr`: `ByteSlice::find`, `find_byte`, `find_byteset`.
<!-- template:begin outbox:skill-workspace-crates-outbox -->
- **Direct SIMD byte or substring search** — `memchr`: `memchr::memmem::Finder` and `memchr2`; benchmark escape-heavy inputs before replacing a simple loop.
- **Fast UTF-8 validation on hot byte buffers** — `simdutf8`: validate raw wire bytes before borrowing `&str`.
<!-- template:end outbox:skill-workspace-crates-outbox -->
- **Stack or streaming hex encoding and decoding** — `const-hex`: `const_hex::display`, `const_hex::Buffer::<N>`, `decode_to_array`, `encode_to_slice`.
- **Standard and URL-safe Base64** — `base64`: decode into caller-owned slices or stack buffers on hot paths.
- **Reusable `serde` field adapters** — `serde_with`: `#[serde_as]` / `DeserializeAs`; never use `PickFirst` or `OneOrMany` on config fields that may carry secrets.
- **Field-path-preserving `serde` errors and URL-encoded forms** — `serde_path_to_error`, `serde_html_form`: use instead of manual form/path wrappers.
- **Typed HTTP headers** — `axum-extra`: `axum_extra::headers::HeaderMapExt` for standard typed headers; do not use for `Bearer` or `Retry-After` (see rejected list).
- **URL parsing and normalization** — `url`: keep cross-crate URI/origin policy in `crates/shared-rules`.
<!-- template:begin http-idempotency:skill-workspace-crates-idempotency -->
- **HTTP Structured Field Values (RFC 8941)** — `sfv`: parse zero-copy via `Parser::new(...).parse_item::<BareItemFromInput<'_>>()` without enabling `parsed-types`.
- **RFC 3986 URI / RFC 3987 IRI validation and resolution** — `iri-string`: use `UriStr` / `IriStr` for validated URI/IRI syntax.
<!-- template:end http-idempotency:skill-workspace-crates-idempotency -->
<!-- template:begin messaging:skill-workspace-crates-messaging -->
- **Percent-encoding for URI fragments, paths, or query components (RFC 3986)** — `percent-encoding`: use instead of custom byte escaping.
<!-- template:end messaging:skill-workspace-crates-messaging -->
<!-- template:begin webhooks:skill-workspace-crates-webhooks -->
- **HTTP-date parsing and formatting (RFC 9110)** — `httpdate`: `parse_http_date` / `fmt_http_date` for `Date` or `Retry-After` HTTP-date values.
<!-- template:end webhooks:skill-workspace-crates-webhooks -->
<!-- template:begin temporal:skill-workspace-crates-temporal -->
- **Durable workflow orchestration and worker execution** — `infra-temporal`, `temporalio-sdk`, `temporalio-common`, `temporalio-workflow`, `temporalio-macros`: keep client RPCs behind `infra-temporal` (`default-features = false`; never enable `opentelemetry`, `otel`, `prometheus`, `envconfig`, or `testing`), author workflows/activities with upstream macros, and keep worker dependencies out of `/service`.
- **Type-erased serde values for Temporal payload converters** — `erased-serde`: used by `infra-temporal` finite-float bounded JSON conversion under `default-features = false`.
<!-- template:end temporal:skill-workspace-crates-temporal -->
- **Timestamps, friendly durations, and byte sizes** — `jiff`, `time`, `bytesize`: `jiff` for friendly durations and structured-log `Timestamp`, `time` for `OffsetDateTime` / RFC 3339 wire formatting (with `large-dates` for Go parity), `bytesize` for size configs.
- **CIDRs, SemVer, UUID v4/v7, exact decimals, UTF-8 paths, JSON Patch, CSV, validation** — `ipnet`, `semver`, `uuid`, `rust_decimal`, `camino`, `json-patch`, `csv`, `validator`: prefer over hand-rolled parsers.
- **Unicode normalization, graphemes, case conversion, edit distance, diffs, regex** — `unicode-normalization`, `unicode-segmentation`, `heck`, `strsim`, `similar`, `regex`: avoid `regex` on hot paths or small fixed HTTP/gRPC grammars where a byte parser or `bstr`/`memchr` suffices.

## Retries, randomness, iterators, and `std`

- **Exponential backoff with jitter and reset** — `backon`: `ExponentialBuilder`; `with_jitter()` adds up to `+100%` over `with_max_delay`, so set `with_max_delay` to half the hard ceiling and clamp when a strict upper bound is required.
- **Fast unbiased non-crypto PRNG and jitter** — `fastrand`: `fastrand::u32`; never use for cryptographic secrets, tokens, or nonces.
- **Zero-alloc iterator formatting, dedup, and fallible folds** — `itertools`: `.format(...)`, `.unique()`, `process_results`, `.join(...)`.
- **One-off infallible RAII cleanup guards** — `std::mem::DropGuard`: built into Rust 1.100 `std`; do not add `scopeguard`.
- **Lazy or fallible one-time initialization** — `std::sync::OnceLock`, `std::sync::LazyLock`: pair `OnceLock` with `parking_lot::Mutex<()>` for fallible init; do not re-add `once_cell`.

## Secrets and cryptography

- **Opaque secret wrappers preventing `Debug`/`Display` leaks** — `secrecy`: `SecretString`, `SecretBox`.
- **Scrubbing secret buffers on drop, growth, and error paths** — `zeroize`: pre-size `Zeroizing<Vec<u8>>` or stack `Zeroizing<[u8; N]>` before reading so reallocations do not leave copies.
- **HMAC, AEAD, constant-time compare, digests, and CSPRNG** — `aws-lc-rs`, `sha2`: keep `ring` off so the workspace shares one crypto provider (`aws-lc-rs`); use `sha2` where `Digest` trait interop is required.

For already-rejected crates and benchmark thresholds, see [backend-library-selection.md](../../../../docs/backend-library-selection.md).
