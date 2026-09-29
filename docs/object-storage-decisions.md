# Object storage decisions

<!-- template:begin object-storage:docs-object-storage-decisions -->
Stage 10.9 library, provider, and failure decisions, recorded 2026-09-29.
[Guide](object-storage.md) owns adoption and observable behavior. This record
retains the accepted choices and their reopen conditions. The comparison
behind them, including the Go sibling template's findings, is the stage's
research synthesis in the template repository
(`specs/s3-object-storage/research/synthesis.md`).

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| `aws-sdk-s3` 1.150.0 with default features off, only `rt-tokio` and `default-https-client`; `aws-smithy-http-client` 1.4.2 (`rustls-aws-lc`) builds the HTTPS client; `aws-smithy-checksums` 0.65.0 names the checksum-mismatch error | `object_store` 0.14.2 is far lighter (+4 lock packages) but retries a create-only put answered 5xx or 429 regardless of idempotency, so a create the provider applied returns `AlreadyExists`; it maps every 409 to `AlreadyExists`, leaves throttling and timeouts untyped, takes only in-memory put payloads, validates no response checksum, and its `aws` feature turns on HTTP/2 for every reqwest client in the workspace. `opendal` 0.59.3 ships a breaking 0.x minor every one to two months, maps every transport failure to one kind, puts the request URL (key included) in error context, and supports only CRC32C and MD5. `rust-s3` needs reqwest 0.12, ring, and an MPL-2.0 dependency. `minio` needs reqwest 0.12 and ring. | +30 lock packages with two small duplicates (`const-oid` 0.10.2, `spin` 0.10.1), no ring, OpenSSL, or native-tls. Three feature edges for the initializer's lock projection. Exact `=` pins: the SDK releases weekly and yanked one release on 2026-09-14 (awslabs/aws-sdk-rust#1459). Reopen for `object_store` if create-only and typed unknown outcomes stop being required, or it fixes the retry and 409 mapping. |
| Default SDK features stay off | Default `rustls` selects the legacy hyper 0.14, rustls 0.21, and ring stack: 12 duplicate versions. `sigv4a` serves multi-region access points; `aws-config` is the ambient credential chain. | The HTTPS client verifies with `rustls-native-certs` (the system store; on Linux the same roots `rustls-platform-verifier` reads) and turns on rustls `prefer-post-quantum`, a rustls default feature, for the whole binary. Reopen if either becomes observable in a deployment. |
| `BehaviorVersion::v2026_01_12()` in code | The `behavior-version-latest` feature would let an SDK bump change retry, timeout, and proxy defaults silently. | Moving it is a reviewed change with the version bump. |
| No multipart and no transfer manager | No consumer exceeds 2 MiB. `aws-sdk-s3-transfer-manager` 0.2.0 is a developer preview that needs default SDK features (+79 packages). Hand-rolled Create/UploadPart/Complete/Abort would own provider differences: R2 requires equal non-final parts and conditions at Create, and Railway has no lifecycle rule to clean abandoned uploads. | `max_object_bytes` is capped at R2's single-upload limit, 4.995 GiB. Reopen when a consumer needs larger objects or the transfer manager reaches 1.0 with default features off. |
| One concrete client, no port trait | The template has no second implementation. A feature that needs a test double or a filesystem backend defines its trait at its own boundary. | Reopen when a second backend is a template requirement. |
| No listing, range reads, copy, tagging, user metadata, or presigned PUT | No production consumer calls them: pricing-service's `ListObjectsV2` and `GetObject` are called only by its tests, and its HEAD read-back can compare the size with the SHA-256 it keeps in PostgreSQL. A presigned PUT cannot enforce the size limit. | A feature adds the operation to `ObjectStorage` so it shares admission, retries, and observation. |

Registry and maintenance evidence, fetched 2026-09-29: `aws-sdk-s3` 1.150.0
released 2026-09-25, Apache-2.0, MSRV 1.94.1, about weekly releases.
`object_store` 0.14.2, 2026-09-15, MIT/Apache-2.0. `opendal` 0.59.3,
2026-09-22, Apache-2.0. `aws-sdk-s3-transfer-manager` 0.2.0, 2026-07-18,
developer preview. `rust-s3` 0.37.2, 2026-05-04. `minio` 0.4.0, 2026-04-23.
Every MSRV fits workspace Rust 1.98.

## Failure semantics

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| A closed `ObjectStorageError`: `NotFound`, `AlreadyExists`, `TooLarge`, `Busy`, `Unavailable`, `Rejected`, `OutcomeUnknown`, `Integrity` | The caller's next action differs for each. The Go sibling maps every failed mutation except 412 to unknown outcome, so callers reconcile a definitive 403 forever. | A consumer needs to branch on a class the set merges. |
| A create-only put makes exactly one attempt | A retried create-only put after a lost success meets its own object and answers 412, which would be reported as `AlreadyExists`. The Go sibling has this defect. | Never while create-only maps 412 to `AlreadyExists`. |
| 409, 429, and 503 on a mutation are `Unavailable`; 500, 502, 504, timeouts, and lost responses are `OutcomeUnknown`; other 4xx are `Rejected` | S3 documents 409 `ConditionalRequestConflict` and 503 `SlowDown` as refusals to retry; a 5xx gateway or internal error can follow an applied write. | A provider documents a different meaning for one of these statuses. |
| Three attempts, each delay capped at 1 s, for reads, deletes, and bytes puts without a condition | The SDK default 20 s cap would outlast an interactive `operation_timeout`. Replaying identical bytes is idempotent. A stream cannot be replayed. | A consumer needs a longer backoff under throttling. |
| Reject, do not queue, at `max_concurrency` (default 8) | A hidden queue turns overload into latency. The Go sibling's fixed 4 would shed document-processing's per-request reads. The permit moves into a download, so dropping it releases the slot and the "caller must close" rule disappears. | Measured overload shows a queue would help. |
| `operation_timeout` bounds a call to its response headers; stalled-stream protection bounds a body | A single timer cannot cover a download whose length the operator does not bound in time. | A consumer needs a total transfer deadline. |
| Not a readiness dependency; no startup I/O; `probe()` is opt-in | A bucket probe during a provider outage would evict every replica. document-processing's worker gates on its bucket and can opt in. | Never as a default. |

## Integrity

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| `WhenRequired` for request calculation and response validation on the client; a put that carries CRC64NVME switches that one call to `WhenSupported` | Under `WhenRequired` the SDK sends a named algorithm as a header with no value; the provider then expects a checksum it never gets. Nothing a provider has not proven is sent by default. | An SDK release changes the checksum interceptor. |
| CRC64NVME on every Amazon upload, on R2 bytes uploads only, and on no Railway upload | Amazon documents CRC64NVME full-object checksums and trailers. R2 documents CRC64NVME full-object since 2025-07-03 but no trailer. Railway and Tigris document only a SHA-256 checksum. | A recorded conformance run shows R2 trailers or a Railway checksum accepted. |
| Every get sends `x-amz-checksum-mode: ENABLED`; a missing checksum is not a failure | Objects written by other clients (the Go SDK defaults to CRC32) must stay readable. The Go sibling refuses them. | Never while existing buckets hold such objects. |

## Providers and configuration

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| Providers `amazon_s3`, `cloudflare_r2`, `railway`, and a local-only `local` | Both production consumers use Railway Buckets (Tigris), region `ams`; the Go sibling cannot express them. `local` admits a plaintext emulator for development and tests only. | A new provider with its own endpoint rules. |
| Each provider accepts only its own keys; shapes are admitted by the crate | A key another provider owns is refused instead of ignored. `service-config` owns applicability and ranges; `infra-object-storage`, which builds the client, owns value shapes. | |
| Virtual-hosted addressing for every production provider; no path-style key | Tigris dropped path-style for buckets created on or after 2025-02-19, both production Railway buckets run virtual-hosted, and dotless bucket names keep TLS wildcards valid. | A Railway bucket that requires path-style. |
| Typed credentials: `access_key_id` and environment-only `secret_access_key`; no session token and no ambient chain | The global `AWS_*` variables would bypass typed configuration and block a second store per process; Railway injects per-bucket values; R2 temporary credentials and Amazon STS expire and need a refresher. | An Amazon adopter needs workload identity: add `aws-config` for that provider only. |
| The HTTPS client is built explicitly: no proxy, no redirect | The pinned behavior version's default client reads `HTTP(S)_PROXY`. A signed request must reach only the configured origin. | A deployment needs an egress proxy. |
| Presign is GET only, 1 s to 7 days | SigV4 and R2 cap at 7 days; Railway allows 90. A presigned PUT cannot enforce the size limit. | A consumer needs uploads from clients. |
| Expected bucket owner only for `amazon_s3`, required there | Confused-deputy protection on Amazon; R2 does not implement the header. | A recorded run shows Railway honors it. |

## Observability

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| One histogram, `object_storage_operation_duration_seconds{operation, outcome}`; a get is observed when its download ends | Per-outcome counts are the `_count` series (the cache profile's precedent), and body failures are counted. | An operator question the histogram cannot answer. |
| One `object_storage.<operation>` span without key, bucket, endpoint, or URL; `error.type` is the provider code, status, or transport class | Keys can carry business identifiers; the bucket and endpoint are configuration. | |
| The subscriber keeps `aws_*` targets at INFO or quieter unless `log.level` names one | The SDK logs endpoint parameters, which include the key, at DEBUG and whole requests at TRACE. | The SDK stops logging keys. |

## Ownership and proving surfaces

`infra-object-storage` owns provider admission, the SDK client, the calls,
failure mapping, admission, the probe, and observation. `service-config` owns
the `[object_storage]` section. Bootstrap owns construction, optional probe
registration, and the shutdown drop. The feature owns keys, authorization,
content policy, retention, create-only intent, and presign recipients.

Credential-free proof: unit tests for admission, the key grammar,
classification, and redaction, plus adapter tests over an in-process HTTP
stub in `make test`; versitygw v1.8.0 (Apache-2.0, 31 MB, pinned by digest)
through Compose in the integration job. MinIO is archived upstream with no
official images, Garage has no conditional writes, S3Mock does not validate
presigned URLs, and LocalStack requires an account token even in CI. Live
provider conformance is an ignored test run per provider only with
`OBJECT_STORAGE_CONFORMANCE_WRITES=allow`; it records provider facts instead
of self-attested receipts.

This record does not claim a live provider run, CI result, merge,
publication, or deployment.
<!-- template:end object-storage:docs-object-storage-decisions -->
