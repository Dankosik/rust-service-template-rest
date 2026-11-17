# Object storage decisions

<!-- template:begin object-storage:docs-object-storage-decisions -->
`crates/infra-object-storage`, `crates/config`, and `Cargo.toml` own the client
API, failure mapping, timeouts, and exact SDK pins (moved together in one
reviewed change with the pinned `BehaviorVersion` in code and
[the HTTP client patch](../../vendor/aws-smithy-http-client/PATCHES.md)).
This record keeps only rejected alternatives, provider divergences, and reopen
conditions.

## Library and architecture choices

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| `aws-sdk-s3` + `aws-config` (default features off) over `object_store`, `opendal`, `rust-s3`, `minio`, or `aws-sdk-s3-transfer-manager` | Default SDK features pull legacy `hyper`/`rustls`/`ring` duplicates and ambient `HTTP(S)_PROXY` / `DefaultCredentialsChain` readers; `object_store` maps 409 to `AlreadyExists` and lacks streaming puts and response checksum verification; `opendal` ships frequent breaking 0.x minors and leaks object keys in error context; `transfer-manager` requires SDK default features and `behavior-version-latest`. | `object_store` or `transfer-manager` meets streaming, error-classification, and zero-default-feature constraints at lower dependency cost, or objects exceed single-PUT limits (4.995 GiB on R2). |
| Concrete `ObjectStorage` adapter (GET, PUT, DELETE, presigned GET) without a generic storage trait, multipart, listing, or presigned PUT | Business features own narrow domain traits; presigned PUT cannot enforce byte caps; neither startup nor readiness probes the bucket so a store outage never evicts all replicas. | A second storage backend or client-direct upload becomes an accepted requirement. |
| Single-attempt mutations vs bounded read retries; final-chunk holdback on `Download` | SDK retries keep only the last reply, turning a retried create-only PUT after a lost 200 into `412 AlreadyExists`; Hyper stops polling at `Content-Length`, which would skip the SDK's EOF checksum verification and record `cancelled` unless empty/final chunks settle inside the adapter. | Upstream SDK exposes per-attempt mutation outcomes or native unpolled-body deadline release. |
| Compose `versitygw` for integration tests over MinIO, Garage, S3Mock, or LocalStack | MinIO is archived upstream without official images; Garage lacks conditional writes; S3Mock skips presigned URL verification; LocalStack requires an account token. Live provider conformance stays manual (`OBJECT_STORAGE_CONFORMANCE_WRITES=allow`). | Compose `versitygw` drops a required S3 conditional or checksum behavior. |

## Provider protocol divergences

| Rule | External provider constraint | Reopen condition |
| --- | --- | --- |
| CRC64NVME on Amazon uploads, R2 byte uploads only, and never on Railway/`s3_compatible` | Amazon supports CRC64NVME full-object checksums and streaming trailers; Cloudflare R2 supports full-object CRC64NVME but rejects trailers; Railway Buckets (Tigris) and generic gateways support only SHA-256 or unverified uploads. | Conformance run proves R2 trailers or Railway CRC64NVME acceptance. |
| `x-amz-checksum-mode: ENABLED` on GET without failing on absent/skipped checksums | Existing buckets hold CRC32/unchecksummed objects from other SDKs, and the Rust SDK skips composite `-N` checksums; callers needing end-to-end attestation must verify an authoritative digest over the bytes. | SDK checksum interceptor changes or bucket contents are homogeneous. |
| Virtual-hosted style for named providers (`path_style` only on `s3_compatible`); `expected_bucket_owner` only on non-presigned `amazon_s3` calls | Tigris deprecated path-style for new buckets; R2 ignores bucket-owner headers, and the Rust SDK signs `x-amz-expected-bucket-owner` as a required request header rather than a query parameter. | Provider conformance or SDK presigning changes. |
| Explicit `aws_*` log target cap at `INFO` unless overridden | AWS SDK `DEBUG`/`TRACE` spans log object keys and request payloads. | Upstream SDK redacts request parameters at `DEBUG`. |
<!-- template:end object-storage:docs-object-storage-decisions -->
