# S3-compatible object storage: research and decisions

Stage 10.9. Baseline `2590d29` (main after PR #121). Research date
2026-09-29. This record is the accepted decision artifact for the profile; the
[guide](../../../docs/object-storage.md) and the
[decision record](../../../docs/object-storage-decisions.md) that ship with the
profile are derived from it.

Evidence was gathered in three read-only lanes: Rust crates (registry sources
at exact versions, crates.io metadata, a resolution of every candidate against
this workspace's `Cargo.lock`), providers (Amazon, Cloudflare and Railway
documentation plus a local emulator probe), and the Go sibling template with
the two production consumers. No real bucket, credential, or Railway resource
was read or changed.

## Accepted outcome

`OBJECT_STORAGE=none|s3` selects an optional pack. `none` (the default)
removes it. `s3` retains one crate, `infra-object-storage`, with a concrete
client for one bucket at one fixed endpoint. Three providers are supported as
equals: `amazon_s3`, `cloudflare_r2`, and `railway`. A fourth, `local`, admits
a plaintext emulator in `local` and `development` environments only.

The client offers put (bytes or a sized stream, optionally create-only), get
(a streaming download or bounded bytes), head, delete, presigned GET, and an
opt-in bucket probe. It is inert: selecting the profile opens nothing, and a
configured provider makes no request until a feature calls it.

## Consumers the template must serve

Two production Go services use S3 today, both through AWS SDK for Go v2 and
both against Railway Buckets.

| Consumer | Production operations | Shape |
| --- | --- | --- |
| pricing-service (`pricing-worker`) | Put, Head (read-back verification), Delete (retention) | In-memory JSON evidence, unique keys, bucket `pricing-evidence`, region `auto`, virtual-hosted. Get and ListObjectsV2 exist in `evidence_store.go` but only tests call them. |
| document-processing-service (API and worker) | Put, Get, Delete, HeadBucket readiness in the worker | Whole payloads in memory, at most 2 MiB, bucket `document-processing-results` (Railway region `ams`). |

No other GonkaGate service and nothing in Bitrina uses object storage. No code
references the `model-assets` bucket. Neither consumer uses multipart,
presigned URLs, conditional writes, or listing in production. Both keep their
own SHA-256 of each object in PostgreSQL.

## Library selection

| Candidate (latest, date) | Verdict | Decisive evidence |
| --- | --- | --- |
| `aws-sdk-s3` 1.150.0 (2026-09-25), default features off: `rt-tokio`, `default-https-client` | **Accepted** | Official SDK; typed `if_none_match`/`if_match` on PutObject and CompleteMultipartUpload; CRC32, CRC32C, CRC64NVME, SHA1, SHA256 with full-object or composite types; response validation at EOF; `expected_bucket_owner`; presign with the 7-day cap; per-operation retry override; `SdkError` separates service, timeout, dispatch, and response failures. Default-off resolves to +30 lock packages with two small duplicates (`const-oid` 0.10.2, `spin` 0.10.1) and no ring, OpenSSL, or native-tls. |
| `object_store` 0.14.2 (2026-09-15) | Rejected | Lightest (+4 to +5 packages), but: a `PutMode::Create` answered 5xx/429 is retried whatever the idempotency flag says, so a create that the provider applied returns `AlreadyExists` (`client/retry.rs:405-420`); any 409 maps to `AlreadyExists`; throttling and timeouts are untyped `Generic`; `put` takes an in-memory payload only (#281); no response checksum validation; no create-only multipart (#289); the `aws` feature turns on HTTP/2 for every reqwest client in the workspace. The alternative, `aws-base` with a template-owned `HttpConnector`, trades those gaps for a homegrown transport. |
| `opendal` 0.59.3 (2026-09-22) | Rejected | Breaking 0.x minor every one to two months, +26 packages and three duplicates, transport failures all `Unexpected` with no timeout kind, error context carries the full request URL (key and upload id), checksums limited to CRC32C and MD5. It does have create-only multipart and typed rate-limit and conflict errors. |
| `aws-sdk-s3-transfer-manager` 0.2.0 | Rejected | README: developer preview, not recommended for production; requires default SDK features (hyper 0.14, rustls 0.21, ring: +79 packages, 14 duplicates). |
| `rust-s3` 0.37.2 | Rejected | reqwest 0.12 duplicate, adds ring as a second rustls provider, and pulls `attohttpc` under MPL-2.0, which `deny.toml` does not allow. |
| `minio` 0.4.0 | Rejected | reqwest 0.12, ring, `syn` 1, six duplicates, yearly MinIO-oriented releases; upstream MinIO itself is archived. |

Default SDK features are a trap: `rustls` selects the legacy hyper 0.14,
rustls 0.21 and ring stack (12 duplicate versions). The profile enables only
`rt-tokio` and `default-https-client` and names `aws-smithy-http-client`
(`rustls-aws-lc`, already in the graph through the SDK) to build the HTTP
client itself. `sigv4a` and `aws-config` are off: SigV4a is a multi-region
access point feature, and `aws-config` is the ambient credential chain this
profile does not use.

Known costs, accepted:

- Weekly SDK releases and a yanked release on 2026-09-14
  (awslabs/aws-sdk-rust#1459). Versions are pinned with `=` and move only in a
  reviewed bump.
- The HTTPS client enables rustls `prefer-post-quantum` for the whole binary.
  It is a rustls default feature (`rustls-0.23.45/Cargo.toml`), so every
  rustls client in the process then prefers the X25519MLKEM768 key share,
  which is what rustls ships by default.
- The HTTPS client verifies with `rustls-native-certs` (the system store),
  not `rustls-platform-verifier`. On Linux both read the same system roots.
- The initializer must remove three feature edges when the profile is not
  selected: `digest 0.11.3 -> const-oid`, `hashbrown 0.17.1 -> foldhash`,
  `hyper-rustls 0.27.9 -> rustls-native-certs`.
- Build time and binary size were not measured locally: the host had under
  7 GiB of free disk. The first CI run records the release image build time
  and binary size against main instead, and the decision record carries those
  numbers.

## Decisions

### Port shape

| Decision | Go template | Rust decision and reason |
| --- | --- | --- |
| One concrete client per bucket, no port trait | `objectstorage.Store` interface plus adapter | Same as the cache profile: the template has no second implementation to abstract over. A feature that wants a test double or a filesystem backend defines its trait at its own boundary. |
| Operations | Upload, Download, Metadata, Delete, PresignGet | The same five plus `probe`. Listing, copy, range reads, tagging, and user metadata are not in the pack: no production consumer calls them. A feature that needs one adds it beside the others so it shares admission, retries, and observation. |
| Keys | UTF-8, 1..1024 bytes | `ObjectKey`, a validated type: 1..1024 bytes of `[A-Za-z0-9._~-]` in `/`-separated segments, no empty, `.` or `..` segment, no leading or trailing `/`. R2 NFC-normalizes keys, so two distinct Unicode keys can collide; both consumers' keys already fit the grammar. This restores the Go spec's grammar that the Go simplification dropped without a recorded reason. |
| Upload body | `io.Reader` plus declared size | `Bytes` (replayable, so retryable) or a stream with a declared length. The declared length is checked against `max_object_bytes` before any I/O. |
| Download | Body owns the admission token; the caller must close it | The admission permit moves into the download; dropping it releases the slot and the connection, so the caller-must-close rule disappears. A download succeeds only at EOF, after SDK checksum validation. `bytes()` collects a bounded body. A `Content-Range` response is an integrity failure. |
| Object size | 80,000 MiB ceiling from 8 MiB parts × 10,000 | No multipart, so the ceiling is the smallest documented single-PUT limit across the providers, R2's 4.995 GiB (5,363,466,240 bytes). `max_object_bytes` defaults to 8 MiB. Head reports the provider's size even above the limit (Go refuses, F13); get refuses it with `TooLarge`. |
| Create-only | `If-None-Match: *`, single PUT ≤ 8 MiB only | `If-None-Match: *` on every size (single PUT only), one attempt. A 412 on that single attempt is `AlreadyExists`. |

### Failure semantics

A closed error enum. Each variant tells the caller what it may do next:

| Variant | Meaning |
| --- | --- |
| `NotFound` | The object does not exist (`NoSuchKey`, `NotFound`, 404 on get or head). Delete of a missing key succeeds, as S3 does. |
| `AlreadyExists` | A create-only put found the key (412 on its only attempt). |
| `TooLarge` | A declared or reported size exceeds `max_object_bytes`. Nothing was sent. |
| `Busy` | The process admission limit is full. Nothing was sent. |
| `Unavailable` | Not applied and transient: any failed read, or a mutation the provider refused before applying it (429, 503, 409 `ConditionalRequestConflict`). Retrying is safe. A mutation's transport failure is not here: the SDK cannot say whether the request reached the provider. |
| `Rejected` | Not applied and permanent: another 4xx such as 400, 403, or 404 `NoSuchBucket`. Configuration, credentials, or input are wrong. |
| `OutcomeUnknown` | A mutation may or may not have taken effect: a timeout, a lost response, or a 500/502/504. The caller reconciles, for example with head. |
| `Integrity` | A checksum mismatch, a partial or range response, or a malformed response. |

Go maps every mutation failure except 412 to unknown outcome (F4), which makes
callers reconcile a definitive 403 forever. A dropped future has no variant: in
Rust, dropping the future is the cancellation, and a dropped mutation's
outcome is unknown by construction.

### Retries, timeouts, admission

| Decision | Choice and reason |
| --- | --- |
| Retry | The SDK standard retryer, three attempts with each delay capped at 1 s (the SDK default of 20 s would outlast an interactive timeout), for get, head, and the probe. Put and delete run exactly one attempt through a per-operation `config_override`: the SDK keeps only the last attempt's reply, so after a retry a refusal could hide an applied earlier attempt, and a 412 could answer a retry of this call's own lost success (Go F3). The independent review found the first case; the original Go spec (D6) had also chosen one-attempt mutations. |
| Timeouts | `object_storage.operation_timeout` bounds one call from start to response headers, retries included: default `5s` (pricing's put, head, and delete budgets), inclusive `1s` to `15m`. Connect is the SDK's 3.1 s from the pinned behavior version. A download body is bounded by the SDK's stalled-stream protection (no progress for 5 s fails it; stated in code because an explicit config takes the builder's 20 s), not by the operation timeout. A streamed upload is held to its declared length, because hyper cuts a longer body at `Content-Length` silently. |
| Behavior version | `BehaviorVersion::v2026_01_12()` in code, not the `behavior-version-latest` feature, so an SDK bump cannot silently change retry, timeout, or proxy defaults. |
| Admission | Reject, do not queue: `Semaphore::try_acquire_owned` refuses excess work with `Busy`. `object_storage.max_concurrency`, default 8, inclusive 1 to 512. Go's fixed 4 is too low for document-processing's per-request reads. The permit is held through a download's body. Worst-case buffered memory is `max_concurrency × max_object_bytes` (64 MiB by default); the guide states the formula. |
| Readiness | Not a readiness dependency, and startup performs no I/O. `probe()` returns a `HeadBucket` probe that a service registers only when its business outcome requires storage (document-processing's worker does). A bucket probe during a provider outage would otherwise evict every replica. |

### Integrity

- The SDK computes and validates checksums; the adapter computes none.
  Both `request_checksum_calculation` and `response_checksum_validation` are
  `WhenRequired` on the client, so nothing the provider has not proven is
  sent by default. Under `WhenRequired` the SDK sends a named algorithm as a
  header without computing it (found while implementing), so a put that
  carries CRC64NVME switches that one call to `WhenSupported`.
- Uploads name CRC64NVME explicitly where the provider accepts it: always on
  Amazon (header for bytes, `aws-chunked` trailer for a stream); on R2 only
  for a bytes body, where the SDK sends it as a signed header (R2 supports
  CRC64NVME full-object since 2025-07-03; its trailer support is undocumented);
  never on Railway until a conformance run proves Tigris accepts it (Tigris
  documents only a SHA-256 checksum).
- Downloads send `x-amz-checksum-mode: ENABLED` and the SDK validates a
  full-object checksum at EOF when the provider returns one. A missing
  checksum is not a failure: objects written by other clients (Go SDK v2
  defaults to CRC32) must stay readable (Go F6).
- Consumers keep their own content digest where the business needs it; both
  production consumers already store SHA-256 in PostgreSQL.

### Providers and configuration

| Provider | Endpoint | Region | Addressing | Other |
| --- | --- | --- | --- | --- |
| `amazon_s3` | Empty; the SDK resolves the regional endpoint | Required, commercial (`^[a-z]{2}-[a-z]+-\d+$`; no `gov` or `cn`) | Virtual-hosted | `expected_bucket_owner` required (12 digits), sent on every call. Not in presigned URLs: the Rust SDK signs it as a header the recipient would have to send (the Go SDK hoists it into the query) |
| `cloudflare_r2` | `https://<32 hex>[.eu\|.fedramp].r2.cloudflarestorage.com`, exact origin | `auto` | Virtual-hosted | No expected-owner header: R2 does not implement it |
| `railway` | Required HTTPS origin, taken from the bucket's `ENDPOINT` (`https://t3.storageapi.dev` today) | Default `auto`; the bucket's `REGION` | Virtual-hosted | Railway Buckets run on Tigris; `BUCKET` is the hashed S3 name, never `RAILWAY_BUCKET_NAME` |
| `local` | `http://` or `https://` origin | Default `us-east-1` | Path-style | `app.env` must be `local` or `development`; for an emulator only |

- Bucket names are dotless DNS names (`^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$`) so
  virtual-hosted TLS wildcards hold, with Amazon's reserved prefixes and
  suffixes refused for `amazon_s3`.
- Path-style is not a key. Tigris dropped path-style for buckets created on
  or after 2025-02-19, and both production Railway buckets run
  virtual-hosted. Reopen if a Railway bucket must use path-style.
- Credentials are typed configuration, not the global `AWS_*` variables (Go
  F7): `object_storage.access_key_id` (an identifier, file or environment)
  and `object_storage.secret_access_key` (`SecretString`, environment only,
  refused in a file by the secret-name guard). On Railway the service maps
  `APP__OBJECT_STORAGE__*` to `${{Bucket.X}}` references. The client config
  is built directly, never from `aws-config`, so no ambient environment,
  profile file, or IMDS can change the endpoint, region, retry, checksum mode,
  or credentials.
- No session token and no ambient credential chain. Railway has neither; R2
  temporary credentials and Amazon STS expire and need a refresher. Reopen
  with `aws-config` when an Amazon adopter needs workload identity.
- Presign is GET only, 1 second to 7 days, the cross-provider cap (Railway
  allows 90 days). The URL is a bearer credential: its type redacts `Debug`,
  and it is never logged.
- The HTTP client is built explicitly: rustls with aws-lc-rs, no proxy (the
  pinned behavior version would otherwise read `HTTP(S)_PROXY`), no redirect
  following (the hyper client follows none). A signed request must reach only
  the configured authority.

### Observability

- One histogram, `object_storage_operation_duration_seconds`, with the closed
  labels `operation` (`put`, `get`, `head`, `delete`, `presign_get`, `probe`)
  and `outcome` (`ok`, `not_found`, `already_exists`, `too_large`, `busy`,
  `unavailable`, `rejected`, `outcome_unknown`, `integrity`, `cancelled`). A
  get is observed when its download finishes (EOF, failure, or drop), so body
  failures are counted. Counts per outcome are the `_count` series; there are
  no parallel counters (the cache profile's precedent).
- One `object_storage.<operation>` span per call with `otel.kind = client`,
  `outcome`, and `error.type`. No key, bucket, endpoint, URL, or provider
  message is ever recorded.
- The SDK logs endpoint parameters, which include the object key, at DEBUG
  and the whole request at TRACE. When the profile is retained, the process
  subscriber (both binaries) keeps every `aws_*` target at INFO or quieter
  except for targets `log.level` names, so a global `debug` never
  records or exports keys. Features still keep personal data out of keys.

### Proof

- Credential-free, in ordinary `make test`: configuration admission, the key
  grammar, the error classification, `Debug` redaction, and adapter behavior
  over an in-process HTTP stub on the `local` provider (412 on the only
  attempt, one request for a failed create-only put, three for a failed get,
  a checksum mismatch at EOF, a range response, admission refusal, size
  refusal, presign bounds).
- Credential-free, in the integration job: versitygw v1.8.0, pinned by
  digest (Apache-2.0, 31 MB, active). It passed all 28 protocol probes,
  including create-only PUT and multipart, CRC64NVME, SDK trailers, and
  presign with real signature and expiry checks. MinIO upstream is archived
  with no official images; Garage has no conditional writes; S3Mock does not
  validate presigned URLs; LocalStack now requires an account token even in
  CI.
- Live providers: `make test-object-storage-conformance PROVIDER=amazon_s3|cloudflare_r2|railway`
  runs one ignored test against an operator-supplied bucket under a unique
  prefix, and only when `OBJECT_STORAGE_CONFORMANCE_WRITES=allow` is set. It
  records what the provider actually does (checksum echo, 412, presign
  expiry, expected-owner handling, 404 shape) instead of self-attested
  receipts (Go F10). One provider's result never qualifies another. These
  runs require separate authorization and were not executed for this stage.

## Rejected or deferred

| Item | Why | Reopen when |
| --- | --- | --- |
| Multipart upload and a lifecycle rule for abandoned uploads | No consumer exceeds 2 MiB; aws-sdk-rust has no stable transfer manager, and hand-rolled Create/UploadPart/Complete/Abort is template-owned machinery with provider differences (R2's equal-part rule, create-only at Create on R2, no lifecycle on Railway). | A consumer needs objects above the single-PUT ceiling, or the transfer manager reaches 1.0 with default features off. |
| Listing | Pricing's `List` has no production caller. | A feature needs prefix listing; add it to the client beside the others. |
| User metadata (`x-amz-meta-*`) | Pricing verifies with a HEAD read-back of its own metadata; its rewrite can compare the HEAD size with the SHA-256 it keeps in PostgreSQL. | A consumer must store metadata with the object. |
| Filesystem backend | Development-only in document-processing; production is S3 only. The `local` provider plus an emulator covers local development. | A deployment without object storage must run the same code. |
| Ambient AWS credential chain, session tokens | See configuration. | An Amazon adopter needs workload identity. |
| Readiness by default | See admission. | Never as a default; a service opts in. |

## Go template findings (not changed here)

The Go sibling was not modified. These are recorded for its maintainers:

| ID | Location | Finding |
| --- | --- | --- |
| F1 | `internal/objectstorage/store.go:55-60` | Key validation regressed from the spec's ASCII grammar to any UTF-8; R2 normalization can merge keys; control characters, a leading `/`, and `..` segments are accepted. |
| F2 | `internal/infra/s3/client.go:57` | The region pattern admits `us-gov-*` and `cn-*` although the guide says commercial regions. |
| F3 | `internal/infra/s3/client.go:163-166`, `store.go:49-66` | The standard retryer covers create-only puts; a seekable body is retried, and a retry after a lost success reports `ErrAlreadyExists` for an object this call created. |
| F4 | `internal/infra/s3/store.go:254-263` | Definitive 400/403/404 `NoSuchBucket` rejections are reported as outcome unknown. |
| F5 | `internal/infra/s3/store.go:298-310` | Any non-EOF body error becomes `ErrIntegrity`, conflating transport loss with a checksum mismatch. |
| F6 | `internal/infra/s3/store.go:63-65,108-111` | Download requires a CRC64NVME full-object echo, so objects written by other clients (Go SDK default CRC32) cannot be read. |
| F7 | `internal/infra/s3/client.go:121-130` | Static credentials come from global `AWS_*` variables, bypassing typed configuration and blocking a second store per process. |
| F8 | `internal/config/object_storage_config.go` vs `client.go:193-220` | Two validation owners: configuration accepts values the adapter later rejects. |
| F9 | commit `010e84c9d` | No object-storage metrics or spans after the simplification; busy rejections are invisible. |
| F10 | `test/s3conformance/conformance_test.go:34-47` | Conformance "receipts" are self-attested environment strings. |
| F11 | `internal/infra/s3/client_test.go:355-397` | Tests fake the SDK interfaces; nothing exercises signing, checksum validation, or retries over HTTP. |
| F12 | `internal/objectstorage/store.go:34-36` | The create-only size limit is an adapter detail that surfaces to callers as `ErrInvalid`. |
| F13 | `internal/infra/s3/store.go:231-237` | Metadata fails with `ErrTooLarge` instead of reporting the size. |
| F14 | `internal/infra/s3/client.go:56` | The bucket pattern omits Amazon's reserved prefixes and suffixes. |
| F15 | `docs/s3-compatible-object-storage.md:49-51` | The guide's retry statement ignores non-seekable and conditional bodies. |
| F16 | `internal/infra/s3` | Railway Buckets cannot be configured: R2 requires an `r2.cloudflarestorage.com` host and Amazon forbids an endpoint. |

## Open provider facts for conformance

Railway documents no checksum algorithms, conditional-write behavior, part
limits, rate limits, or expected-owner handling; Tigris (behind it) rejects
unsigned `x-amz-*` headers since 2026-09-28. R2 documents no trailer support
and no `x-amz-checksum-mode` on GET. The conformance test records each of
these per provider; the adapter does not depend on any of them.
