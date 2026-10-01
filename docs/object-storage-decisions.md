# Object storage decisions

<!-- template:begin object-storage:docs-object-storage-decisions -->
Stage 10.9 library, provider, and failure decisions, recorded 2026-09-29 and
amended 2026-10-01 (workload identity, the generic provider, the read attempt
bound, body types, and failure visibility).
[Guide](object-storage.md) owns adoption and observable behavior. This record
retains the accepted choices and their reopen conditions. The comparison
behind them, including the Go sibling template's findings, is the stage's
research synthesis in the template repository
(`specs/s3-object-storage/research/synthesis.md`).

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| `aws-sdk-s3` 1.150.0 with default features off, only `rt-tokio` and `default-https-client`; `aws-smithy-http-client` 1.4.2 (`rustls-aws-lc`) builds the HTTPS client; `aws-smithy-checksums` 0.65.0 names the checksum-mismatch error | `object_store` 0.14.2 is far lighter (+4 lock packages) but retries a create-only put answered 5xx or 429 regardless of idempotency, so a create the provider applied returns `AlreadyExists`; it maps every 409 to `AlreadyExists`, leaves throttling and timeouts untyped, takes only in-memory put payloads, validates no response checksum, and its `aws` feature turns on HTTP/2 for every reqwest client in the workspace. `opendal` 0.59.3 ships a breaking 0.x minor every one to two months, maps every transport failure to one kind, puts the request URL (key included) in error context, and supports only CRC32C and MD5. `rust-s3` needs reqwest 0.12, ring, and an MPL-2.0 dependency. `minio` needs reqwest 0.12 and ring. | +30 lock packages (+4 more for `aws-config`, below) with two small duplicates (`const-oid` 0.10.2, `spin` 0.10.1), no ring, OpenSSL, or native-tls. Three feature edges for the initializer's lock projection. Exact `=` pins: the SDK releases weekly and yanked one release on 2026-09-14 (awslabs/aws-sdk-rust#1459). Reopen for `object_store` if create-only and typed unknown outcomes stop being required, or it fixes the retry and 409 mapping. |
| `aws-config` 1.12.0 with default features off, only `rt-tokio`, for the three workload identity providers; `aws-credential-types` 1.3.0 names the credential-load error | Hand-written STS, container endpoint, and IMDSv2 clients would own token rotation, the endpoint allowlist, and expiry handling the SDK already tests. The default chain (`DefaultCredentialsChain`) also reads environment access keys, profile files, SSO, and `credential_process`, and is built asynchronously. | +4 lock packages (`aws-config`, `aws-sdk-sts`, `aws-smithy-query`, `urlencoding`), no new duplicate. Default features stay off: `sso` adds two SDK clients, `credentials-process` spawns commands, `default-https-client` a second client. Unproven without an AWS account. Reopen if a consumer needs an assumed role chain or SSO. |
| `sync_wrapper` 1.0.2 around a streamed upload body | The SDK requires a `Sync` body and axum's request `Body` is not one, so a handler could not stream its request into a put. `SyncWrapper` is what axum and reqwest use for the same bound; a body is polled only through `&mut`. | Already in the lock through axum, tower, and reqwest. |
| Default SDK features stay off | Default `rustls` selects the legacy hyper 0.14, rustls 0.21, and ring stack: 12 duplicate versions. `sigv4a` serves multi-region access points. | The HTTPS client verifies with `rustls-native-certs` (the system store; on Linux the same roots `rustls-platform-verifier` reads) and turns on rustls `prefer-post-quantum`, a rustls default feature, for the whole binary. Reopen if either becomes observable in a deployment. |
| `BehaviorVersion::v2026_01_12()` in code | The `behavior-version-latest` feature would let an SDK bump change retry, timeout, and proxy defaults silently. | Moving it is a reviewed change with the version bump. |
| No multipart and no transfer manager | No consumer exceeds 2 MiB. `aws-sdk-s3-transfer-manager` 0.2.0 is a developer preview that needs default SDK features (+79 packages). Hand-rolled Create/UploadPart/Complete/Abort would own provider differences: R2 requires equal non-final parts and conditions at Create, and Railway has no lifecycle rule to clean abandoned uploads. | `max_object_bytes` is capped at R2's single-upload limit, 4.995 GiB. Reopen when a consumer needs larger objects or the transfer manager reaches 1.0 with default features off. |
| One concrete client, no port trait | The template has no second implementation. A feature that needs a test double or a filesystem backend defines its trait at its own boundary. | Reopen when a second backend is a template requirement. |
| No listing, range reads, copy, tagging, user metadata, or presigned PUT | No production consumer calls them: pricing-service's `ListObjectsV2` and `GetObject` are called only by its tests, and its HEAD read-back can compare the size with the SHA-256 it keeps in PostgreSQL. A presigned PUT cannot enforce the size limit. | A feature adds the operation to `ObjectStorage` so it shares admission, retries, and observation. |

Registry and maintenance evidence, fetched 2026-09-29: `aws-sdk-s3` 1.150.0
released 2026-09-25, Apache-2.0, MSRV 1.94.1, about weekly releases.
`aws-config` 1.12.0, 2026-09-04, Apache-2.0, MSRV 1.94.1 (fetched
2026-10-01; resolves with `aws-sdk-s3` 1.150.0 without moving a smithy crate).
`object_store` 0.14.2, 2026-09-15, MIT/Apache-2.0. `opendal` 0.59.3,
2026-09-22, Apache-2.0. `aws-sdk-s3-transfer-manager` 0.2.0, 2026-07-18,
developer preview. `rust-s3` 0.37.2, 2026-05-04. `minio` 0.4.0, 2026-04-23.
Every MSRV fits workspace Rust 1.98.

## Failure semantics

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| A closed `ObjectStorageError`: `NotFound`, `AlreadyExists`, `TooLarge`, `Busy`, `Unavailable`, `Rejected`, `OutcomeUnknown`, `Integrity` | The caller's next action differs for each. The Go sibling maps every failed mutation except 412 to unknown outcome, so callers reconcile a definitive 403 forever. | A consumer needs to branch on a class the set merges. |
| One read attempt is bounded at half of `operation_timeout`; a mutation's single attempt gets all of it | The SDK sets no attempt bound, so an attempt that hung before its response headers used the whole budget and the retries never ran. Half leaves room for one more attempt and its backoff. | A consumer needs a different share, or its own key. |
| A failed credential load is `Unavailable` for every call | The SDK reports it as a dispatch failure, which for a mutation read as `OutcomeUnknown` although nothing was sent. The identity endpoint failing is transient; a wrong role shows as repeated `credentials` failures. | |
| A put or delete makes exactly one attempt; reads make up to three, each delay capped at 1 s | The SDK keeps only the last attempt's reply (`aws-smithy-runtime` orchestrator), so after a retry a 503 could hide an earlier attempt that applied the mutation, and a retried create-only put after a lost success meets its own object and answers 412, reported as `AlreadyExists` (the Go sibling has this defect). The SDK default 20 s backoff cap would outlast an interactive `operation_timeout`. | The SDK exposes every attempt's reply, or a consumer needs SDK-level write retries and accepts reporting them as unknown outcomes. |
| 409, 429, 503, and S3's `400 RequestTimeout` on a mutation are `Unavailable`; 500, 502, 504, timeouts, and lost responses are `OutcomeUnknown`; 501 and other 4xx are `Rejected` | S3 documents 409 `ConditionalRequestConflict`, 503 `SlowDown`, and `RequestTimeout` as refusals to retry; a 5xx gateway or internal error can follow an applied write; 501 is an unsupported header or operation. | A provider documents a different meaning for one of these statuses. |
| A streamed body is held to its declared length | hyper cuts a longer body at `Content-Length` without an error, so a provider that receives no checksum would store a truncated object. | Never. |
| Reject, do not queue, at `max_concurrency` (default 8) | A hidden queue turns overload into latency. The Go sibling's fixed 4 would shed document-processing's per-request reads. The permit moves into a download, so dropping it releases the slot and the "caller must close" rule disappears. | Measured overload shows a queue would help. |
| A `Download` is an `http_body::Body` of exactly the object's size and holds back the chunk that completes the object until the provider's body has ended | A feature returns it as a response body without an adapter. hyper stops polling a body at `Content-Length`, so the end, where the SDK validates the checksum, would never be observed: the download would record `cancelled` and a failed check would go unseen after the whole object was sent. | Never. |
| `operation_timeout` bounds a call to its response headers; stalled-stream protection with a 5 s grace (stated in code: an explicit config would otherwise take the builder's 20 s) bounds a body | A single timer cannot cover a download whose length the operator does not bound in time. | A consumer needs a total transfer deadline. |
| Not a readiness dependency; no startup I/O; `probe()` is opt-in | A bucket probe during a provider outage would evict every replica. document-processing's worker gates on its bucket and can opt in. | Never as a default. |

## Integrity

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| `WhenRequired` for request calculation and response validation on the client; a put that carries CRC64NVME switches that one call to `WhenSupported` | Under `WhenRequired` the SDK sends the named algorithm (`x-amz-sdk-checksum-algorithm`) but computes no checksum value, so the provider would expect a checksum it never gets. Nothing a provider has not proven is sent by default. | An SDK release changes the checksum interceptor. |
| CRC64NVME on every Amazon upload, on R2 bytes uploads only, and on no Railway upload | Amazon documents CRC64NVME full-object checksums and trailers. R2 documents CRC64NVME full-object since 2025-07-03 but no trailer. Railway and Tigris document only a SHA-256 checksum. | A recorded conformance run shows R2 trailers or a Railway checksum accepted. |
| Every get sends `x-amz-checksum-mode: ENABLED`; a missing checksum is not a failure | Objects written by other clients (the Go SDK defaults to CRC32) must stay readable. The Go sibling refuses them. | Never while existing buckets hold such objects. |

## Providers and configuration

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| Providers `amazon_s3`, `cloudflare_r2`, `railway`, `s3_compatible`, and a local-only `local` | Both production consumers use Railway Buckets (Tigris), region `ams`; the Go sibling cannot express them. `local` admits a plaintext emulator for development and tests only. `s3_compatible` (2026-10-01) lets a derived service reach any other store over HTTPS without editing the crate; it receives no checksum and no expected owner, because nothing about the store is proven. | A store that needs a feature the common subset lacks becomes a named provider after a conformance run. |
| `amazon_s3` admits any region and any valid bucket name | The first version refused the GovCloud and China partitions and Amazon's reserved bucket-name affixes. Neither closed a threat: credentials decide which partition answers, and S3 itself refuses a name it reserves. The affix list would also go stale. | |
| Each provider accepts only its own keys; shapes are admitted by the crate | A key another provider owns is refused instead of ignored. `service-config` owns applicability and ranges; `infra-object-storage`, which builds the client, owns value shapes. | |
| Virtual-hosted addressing for every named production provider; `path_style` only for `s3_compatible` | Tigris dropped path-style for buckets created on or after 2025-02-19, both production Railway buckets run virtual-hosted, and dotless bucket names keep TLS wildcards valid. A self-hosted gateway often serves no per-bucket host. | A Railway bucket that requires path-style. |
| Typed credentials: `credentials = "access_key"` with `access_key_id` and environment-only `secret_access_key`, or for `amazon_s3` `credentials = "workload_identity"`; no static session token and no default chain | The global `AWS_*` access-key variables would bypass typed configuration and block a second store per process; Railway injects per-bucket values. On AWS a long-lived IAM user key is the discouraged form, so the workload's role is selectable by one explicit key: web identity (EKS IRSA), the container endpoint (ECS, EKS Pod Identity), then the instance profile, in the SDK's order and with its refresh. A static session token would expire with no refresher. | A consumer needs R2 temporary credentials or an assumed role chain. |
| The HTTPS client is built explicitly: no proxy, no redirect | The pinned behavior version's default client reads `HTTP(S)_PROXY`. A signed request must reach only the configured origin. | A deployment needs an egress proxy. |
| Presign is GET only, 1 s to 7 days | SigV4 and R2 cap at 7 days; Railway allows 90. A presigned PUT cannot enforce the size limit. | A consumer needs uploads from clients. |
| Expected bucket owner only for `amazon_s3`, required there, and not in presigned URLs | Confused-deputy protection on Amazon; R2 does not implement the header. The Rust SDK signs it as a header, not a query parameter, so a presigned URL carrying it would fail without that header; a presigned URL is refused if it needs any header. | A recorded run shows Railway honors it, or the SDK hoists `x-amz-*` headers into presigned queries. |

## Observability

| Decision | Reason | Reopen condition |
| --- | --- | --- |
| One histogram, `object_storage_operation_duration_seconds{operation, outcome}`; a get is observed when its download ends | Per-outcome counts are the `_count` series (the cache profile's precedent), and body failures are counted. | An operator question the histogram cannot answer. |
| One span exported as `S3.<Operation>` without key, bucket, endpoint, or URL; `error.type` is the provider code, status, or transport class | Keys can carry business identifiers; the bucket and endpoint are configuration. `Service.Operation` is the OpenTelemetry name for AWS SDK client spans. `rpc.system`, `rpc.service`, and `rpc.method` stay: their replacements are not stable yet, and the convention keeps the current names until they are. | The RPC conventions are marked stable. |
| The failure event is a WARN for `unavailable`, `rejected`, `outcome_unknown`, and `integrity`; DEBUG for the rest | The caller's error carries no provider detail by design, so at INFO without tracing nothing said why a call failed (`AccessDenied` or `SignatureDoesNotMatch`). `not_found`, `already_exists`, `too_large`, and `busy` answer the caller, and `busy` would flood under load. | |
| The subscriber keeps `aws_*` targets at INFO or quieter; a target `log.level` names is exempt with the targets under it | The SDK logs endpoint parameters, which include the key, at DEBUG and whole requests at TRACE. | The SDK stops logging keys. |

## Ownership and proving surfaces

`infra-object-storage` owns provider admission, the SDK client, the calls,
failure mapping, admission, the probe, and observation. `service-config` owns
the `[object_storage]` section. Bootstrap owns construction, optional probe
registration, and the shutdown drop. The feature owns keys, authorization,
content policy, retention, create-only intent, and presign recipients.

Credential-free proof: unit tests for admission, the key grammar,
classification, and redaction, plus adapter tests over an in-process HTTP
stub in `make test` (including the read attempt bound, a non-`Sync` request
body, the download as a body, and a failed credential load); versitygw v1.8.0 (Apache-2.0, 31 MB, pinned by digest)
through Compose in the integration job. MinIO is archived upstream with no
official images, Garage has no conditional writes, S3Mock does not validate
presigned URLs, and LocalStack requires an account token even in CI. Live
provider conformance is an ignored test run per provider only with
`OBJECT_STORAGE_CONFORMANCE_WRITES=allow`; it records provider facts instead
of self-attested receipts.

This record does not claim a live provider run, a workload identity run in an
AWS account, CI result, merge, publication, or deployment.
<!-- template:end object-storage:docs-object-storage-decisions -->
