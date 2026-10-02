# Object storage

<!-- template:begin object-storage:docs-object-storage -->
`OBJECT_STORAGE=s3` retains an optional client for one S3-compatible bucket at
one fixed endpoint. It is inert by default (`OBJECT_STORAGE=none`).
[Decisions](object-storage-decisions.md) own the library, provider, and
failure-semantics choices.

## What the profile retains

`crates/infra-object-storage` holds one concrete client, `ObjectStorage`, over
`aws-sdk-s3`. A feature calls:

| Call | S3 operation | Notes |
| --- | --- | --- |
| `put(key, body, options)` | `PutObject` | `Bytes` or any `http_body::Body` with a declared length, such as a request body; optional `Content-Type`; optional create-only (`If-None-Match: *`) |
| `get(key)` | `GetObject` | A streaming `Download` that holds an admission slot until its body ends; it is an `http_body::Body`, and `bytes()` collects it |
| `head(key)` | `HeadObject` | Size, content type, last-modified, and ETag |
| `delete(key)` | `DeleteObject` | A missing key is success |
| `presign_get(key, expires_in)` | presigned `GetObject` | 1 second to 7 days; nothing is sent |
| `probe()` | `HeadBucket` | An opt-in readiness probe |

The SDK owns signing, the wire protocol, retries, checksums, and presigning.
The crate admits the provider tuple, bounds object size and concurrency, maps
every result to one closed error set, and observes each call without recording
keys.

It does not add listing, multipart upload, range reads, copy, tagging, user
metadata, presigned PUT, bucket management, a filesystem backend, or a port
trait. The feature owns keys, authorization, content policy, retention,
create-only intent, and who receives a presigned URL. A feature that needs one
more S3 operation adds it to `ObjectStorage` beside the others, so it shares
admission, retries, and observation; it does not hold a raw SDK client.

## Select and configure

`OBJECT_STORAGE=s3` retains the pack; `OBJECT_STORAGE=none` removes it.
Selection alone builds nothing. The `[object_storage]` section is active when
`provider` is not `none`.

| Key | `amazon_s3` | `cloudflare_r2` | `railway` | `s3_compatible` | `local` |
| --- | --- | --- | --- | --- | --- |
| `endpoint` | must be empty; the SDK resolves the regional endpoint | `https://<account id>[.eu\|.fedramp].r2.cloudflarestorage.com` | the bucket's `ENDPOINT` (`https://t3.storageapi.dev` today) | `https://` origin; a port is allowed | `http://` or `https://` origin; a port is allowed |
| `region` | required, such as `eu-central-1` | empty or `auto` | the bucket's `REGION`; `auto` when empty | `us-east-1` when empty | `us-east-1` when empty |
| `expected_bucket_owner` | required 12-digit account id | not accepted | not accepted | not accepted | not accepted |
| `path_style` | not accepted | not accepted | not accepted | `false` by default | not accepted |
| `credentials` | `access_key` or `workload_identity` | `access_key` | `access_key` | `access_key` | `access_key` |
| Addressing | virtual-hosted | virtual-hosted | virtual-hosted | virtual-hosted, or path-style with `path_style = true` | path-style |

Every provider needs `bucket`. With `credentials = "access_key"` (the
default) it also needs `access_key_id` and `secret_access_key`.
`secret_access_key` is environment-only
(`APP__OBJECT_STORAGE__SECRET_ACCESS_KEY`); a nonempty file value fails
startup. A key another provider owns fails startup instead of being ignored.
An endpoint is a bare origin: no path, query, or user information, and a port
only where the table allows one. The bucket is a dotless DNS name, so
virtual-hosted TLS certificates match. `local` is for an emulator and is
accepted only when `app.env` is `local` or `development`.

The S3 client is built from these keys alone. The global `AWS_*` variables,
AWS profile files, and `HTTP(S)_PROXY` never change its endpoint, region,
retries, or checksums, and no redirect is followed, so a signed request
reaches only the configured origin. Only `credentials = "workload_identity"`
reads anything from the platform, and only to obtain credentials.

### Amazon S3 with a workload identity

On AWS, prefer the workload's own role to a long-lived access key:

```toml
[object_storage]
provider = "amazon_s3"
region = "eu-central-1"
bucket = "document-results"
expected_bucket_owner = "123456789012"
credentials = "workload_identity"
```

`access_key_id` and `secret_access_key` must then be empty. The client takes
temporary credentials from the first source the platform provides and renews
them before they expire:

| Platform | Source | What the platform injects |
| --- | --- | --- |
| EKS IAM roles for service accounts | `AssumeRoleWithWebIdentity` at the region's STS endpoint | `AWS_WEB_IDENTITY_TOKEN_FILE`, `AWS_ROLE_ARN`, optional `AWS_ROLE_SESSION_NAME` |
| ECS task roles, EKS Pod Identity | the container credentials endpoint | `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` or `AWS_CONTAINER_CREDENTIALS_FULL_URI`, with `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE` |
| EC2 instance profile | the instance metadata service at `http://169.254.169.254`, IMDSv2 only | nothing; `AWS_EC2_METADATA_DISABLED=true` turns it off |

Environment access keys (`AWS_ACCESS_KEY_ID`), profile files, SSO, and
`credential_process` are not sources, and no profile file is read: the
metadata endpoint is fixed, so `AWS_EC2_METADATA_SERVICE_ENDPOINT` and an
IPv6-only metadata service are not supported. Startup still sends nothing:
the first call loads the credentials. When the source answers that it has
none, the call fails with `Unavailable` and `error.type` `credentials`, and
nothing reaches S3. A source that does not answer inside the call's budget
reads as a timeout instead, which for a put or delete is `OutcomeUnknown`
although nothing was sent. The
providers are the AWS SDK's own (`aws-config`); they log their failures under
the `aws_config` target. This path has no credential-free proof: verify it in
the target account before relying on it.

### Another S3-compatible store

`s3_compatible` reaches a store this page does not name, such as Backblaze B2,
Hetzner Object Storage, or a self-hosted Ceph gateway:

```toml
[object_storage]
provider = "s3_compatible"
endpoint = "https://s3.eu-central-003.backblazeb2.com"
region = "eu-central-003"
bucket = "document-results"
access_key_id = "..."
```

It gets the common S3 subset only: HTTPS, an access key, no upload checksum,
and no expected bucket owner. Set `path_style = true` for a store that serves
no `<bucket>.<endpoint>` host. Nothing about such a store is assumed: run the
live conformance below against it, in particular for create-only puts, before
a feature relies on `AlreadyExists`.

On Railway, give the service the bucket's reference variables:

```text
APP__OBJECT_STORAGE__PROVIDER=railway
APP__OBJECT_STORAGE__BUCKET=${{Bucket.BUCKET}}
APP__OBJECT_STORAGE__ENDPOINT=${{Bucket.ENDPOINT}}
APP__OBJECT_STORAGE__REGION=${{Bucket.REGION}}
APP__OBJECT_STORAGE__ACCESS_KEY_ID=${{Bucket.ACCESS_KEY_ID}}
APP__OBJECT_STORAGE__SECRET_ACCESS_KEY=${{Bucket.SECRET_ACCESS_KEY}}
```

`BUCKET` is the hashed S3 name; `RAILWAY_BUCKET_NAME` is the display name and
is not accepted by S3. Each Railway environment has its own bucket and
credentials. Resetting credentials in Railway invalidates the old pair at
once, with no overlap: redeploy the service right after a reset.

```toml
[object_storage]
provider = "cloudflare_r2"
endpoint = "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com"
bucket = "document-results"
access_key_id = "..."
max_object_bytes = "8 MiB"
max_concurrency = 8
operation_timeout = "5s"
# secret_access_key is environment-only: APP__OBJECT_STORAGE__SECRET_ACCESS_KEY
```

## Use it from a feature

Hold an `ObjectStorage` from composition. Keys are `ObjectKey` values: 1 to
1024 bytes of `[A-Za-z0-9._~-]` in `/`-separated segments, with no empty, `.`,
or `..` segment. The grammar is ASCII because R2 normalizes Unicode keys, so
two distinct keys could name one object. Keys reach provider logs, and the SDK
logs them at DEBUG: never put personal data in a key.

```rust
let key = ObjectKey::new(format!("parse-results/{operation_id}.json"))?;
let options = PutOptions::default()
    .content_type(ContentType::new("application/json")?)
    .create_only();
match storage.put(&key, Bytes::from(json).into(), options).await {
    Ok(()) => {}
    Err(ObjectStorageError::AlreadyExists) => return Ok(Existing),
    Err(ObjectStorageError::OutcomeUnknown) => reconcile_with_head(&key).await?,
    Err(error) => return Err(error.into()),
}
let body = storage.get(&key).await?.bytes().await?;
```

`put` takes `Bytes` (or `Vec<u8>`) directly. A stream uses
`PutBody::stream(len, body)` with any `http_body::Body<Data = Bytes>`,
including a handler's request body
(`PutBody::stream(len, request.into_body())`); a body that yields more or
fewer than `len` bytes fails the put with `Rejected` instead of storing a
truncated object. To check the length exactly, the frame
that completes it is sent only once the stream has ended, so a stream that
delivers its last byte but ends late delays the upload (and fails through the
5 s stall bound or `operation_timeout`).

A `Download` can be read chunk by chunk with `next_chunk()`. It is also an
`http_body::Body` of exactly `metadata().size` bytes, so a handler returns it
as a response body with `Body::new(download)` and its own `Content-Length`.
After a failure every later call returns the same error, so a cut body never
reads as a clean end. The chunk that completes the object is released only
after the provider's body has ended and a returned checksum has been
validated: a reader that stops at the declared length, as an HTTP server
does, never receives a complete object that failed the check. An empty
object's download has already ended when `get` returns.

A streamed download holds its admission slot for as long as its reader takes.
The stall bound watches the provider, not the reader, and the HTTP server sets
no deadline on writing a response body. A client that reads slowly therefore
keeps the slot, and `max_concurrency` such clients make every other call
`Busy`. Choose by who reads:

| Reader | Return the object as |
| --- | --- |
| A client outside the service, object fits in memory | `get(key).await?.bytes().await?`, then the response: the slot is held only while the provider sends |
| A client outside the service, larger object | a presigned URL: the client downloads from the store |
| A caller that reads promptly, such as another service or a proxy that buffers responses | `Body::new(download)` |

## Failures

`ObjectStorageError` is closed. Its variants tell the caller what to do next:

| Variant | Meaning | Caller action |
| --- | --- | --- |
| `NotFound` | The object does not exist (see the note below on Amazon and on `head`) | Business decision |
| `AlreadyExists` | A create-only put found the key | Business decision |
| `TooLarge` | A declared or stored size exceeds `max_object_bytes`; nothing was sent | Refuse the input |
| `Busy` | The admission limit is full; nothing was sent | Shed load, for example HTTP 503 |
| `Unavailable` | Transient: a read failed, the workload identity yielded no credentials (nothing was sent), or the provider refused a mutation before applying it (409, 429, 503, or S3's `400 RequestTimeout`) | Retry later |
| `Rejected` | Permanent: another 4xx, 501, a missing bucket, a streamed body that does not match its length, or an out-of-range presign lifetime | Fix configuration, credentials, or input |
| `OutcomeUnknown` | A mutation may or may not have taken effect: a timeout, a lost response, or a 500, 502, or 504 | Reconcile, for example with `head`, before relying on either state |
| `Integrity` | A checksum mismatch or a range response | Treat the data as unusable |

A put or delete makes exactly one attempt, so the reply the client classifies
is the only one: a refusal really means nothing was applied. A retry could
hide an earlier attempt that applied the mutation behind a later 503, and a
create-only retry after a lost success would meet the object this call created
and answer 412, which would be reported as `AlreadyExists` for the caller's
own object. The caller retries `Unavailable` with its own policy. If a caller retries after
`OutcomeUnknown`, `AlreadyExists` can mean its own earlier attempt; compare
the object (for example its size or a digest the feature keeps) before
deciding. Display and `Debug` never contain a key, bucket, endpoint, URL, or
provider message.

A missing object is `NotFound` only when the provider may say so. Amazon S3
answers 403 instead of 404 for a missing key when the credentials lack
`s3:ListBucket`, which reads as `Rejected`: grant it where `NotFound` matters.
A `head` response has no body, so a missing bucket on `head` also reads as
`NotFound`; `get` and `delete` report it as `Rejected`.

## Retries, timeouts, and admission

- The SDK standard retryer runs up to three attempts, each delay capped at
  1 s, for get, head, and the probe. Put and delete make one attempt (see
  Failures).
- `object_storage.operation_timeout` (default `5s`, `1s` to `15m`) bounds one
  call up to its response headers, retries included. One read attempt gets
  half of it, so an attempt that hangs before its response headers leaves
  room for a retry; the single attempt of a put or delete gets all of it.
  Connect is bounded at 3.1 s, or at the attempt bound when that is shorter.
  A download body is bounded by the SDK's stalled-stream protection: no
  progress for 5 s fails it with `Unavailable`.
- On a request path the handler budget still applies: a call dropped by
  `http.request_timeout` is cancelled, and a cancelled mutation has an unknown
  outcome.
- `object_storage.max_concurrency` (default `8`, `1` to `512`) admits that
  many calls at once and refuses the excess with `Busy`; there is no queue.
  A download holds its slot until its body ends or it is dropped, so a slow
  reader of a streamed download keeps it (see Use it from a feature).
  Presigning and the probe take no slot.
- `object_storage.max_object_bytes` (default `8 MiB`, at most 4.995 GiB, the
  smallest single-upload limit of the supported providers) bounds a put before
  anything is sent and a get before its body is read. `head` reports the real
  size even above it. Buffered reads through `bytes()` can hold up to
  `max_concurrency * max_object_bytes` in memory: 64 MiB with the defaults.

## Integrity

The client computes no checksum itself. Uploads name CRC64NVME, and the SDK
computes it, where the provider is known to accept it:

| Provider | Upload checksum |
| --- | --- |
| `amazon_s3` | CRC64NVME on every put: a header for bytes, an `aws-chunked` trailer for a stream |
| `cloudflare_r2` | CRC64NVME for bytes only, as a signed header; R2 documents no trailer support |
| `railway` | None until a conformance run proves Tigris accepts it |
| `s3_compatible` | None: nothing is assumed about the store |
| `local` | As `amazon_s3` |

Every get asks for the stored checksum, and the SDK validates a full-object
checksum at the end of the body; a mismatch is `Integrity`. An object without
a returned checksum is still readable, because objects written by other
clients (the Go SDK defaults to CRC32) must stay readable. A feature that
needs end-to-end integrity keeps its own digest, as both GonkaGate consumers
already do in PostgreSQL.

## Presigned URLs

`presign_get` signs locally; nothing is sent to the store. The lifetime is 1 second to
7 days, the cross-provider cap (Railway would allow 90 days). The URL is a
bearer credential until it expires: `PresignedUrl` redacts `Debug`, and the
feature hands `expose()` only to the intended recipient and never logs it.
The URL needs no extra header, so it carries no expected bucket owner even on
Amazon S3: the Rust SDK signs that check as a header the recipient would have
to send. There is no presigned PUT, because the provider cannot enforce the
object size limit on it. Under `workload_identity` presigning may first load
the credentials, and the URL stops working when those temporary credentials
expire, whatever lifetime was requested: on AWS that is usually an hour to
twelve hours.

## Readiness and shutdown

Storage is not a readiness dependency, and startup sends nothing. A probe
would turn a provider outage into total unavailability of every replica.
`ObjectStorage::new` admits the configuration and builds the client; a
misconfiguration fails startup before the listener. Bootstrap logs
`object_storage_configured` with the provider, the credential source, and
the limits.

A service that cannot serve without the bucket opts in by pushing the probe in
`crates/service/src/bootstrap/mod.rs`:

```rust
if let Some(storage) = &dependencies.object_storage {
    probes.push(Box::new(storage.probe()));
}
```

The probe is named `object_storage` and sends `HeadBucket`, which needs
list permission on the bucket. It takes no admission slot, so load never
fails readiness, and the readiness refresher bounds it with
`health.probe_budget`. Do not add it to liveness.

Bootstrap drops `Option<ObjectStorage>` inside `Dependencies::close`, after
the HTTP drain; idle connections close with the last clone.

## Observability

The histogram is `object_storage_operation_duration_seconds` with the labels
`operation` (`put`, `get`, `head`, `delete`, `presign_get`, `probe`) and
`outcome` (`ok`, `cancelled`, and each failure: `not_found`,
`already_exists`, `too_large`, `busy`, `unavailable`, `rejected`,
`outcome_unknown`, `integrity`). A get is recorded when its download ends, so
its duration includes the body and a body failure is counted. A dropped call
or download records `cancelled`. Counts per outcome are the `_count` series.

Admission pressure:

```text
sum(rate(object_storage_operation_duration_seconds_count{outcome="busy"}[5m]))
```

Pressure together with long `get` durations points at slow readers of
streamed downloads.

Each call has one `object_storage` span exported as `S3.<Operation>`
(`S3.PutObject`, `S3.GetObject`, `S3.HeadObject`, `S3.DeleteObject`,
`S3.HeadBucket` for the probe, and `S3.PresignGetObject`), the OpenTelemetry
name for an AWS SDK client span. It carries `otel.kind` `client`,
`rpc.system` `aws-api`, `rpc.service` `S3`, `rpc.method`,
`object_storage.outcome`, `error.type`, `otel.status_code`, and, once the
store has answered, `aws.request_id` and `aws.extended_request_id`. On
`amazon_s3` it also carries `cloud.region`; the other providers' signing
region can be a placeholder such as `auto`, so they record none.
`error.type` is the provider's error code (`AccessDenied`, `SlowDown`,
`PreconditionFailed`), the HTTP status when there is no code, or a transport
class (`timeout`, `dispatch`, `response`, `body`, `checksum`, `credentials`).

A failure also emits one event, `object_storage_operation_failed`, with the
operation, outcome, and `error.type`. The error a caller receives carries no
provider detail, so this event is where an operator reads why a call failed.
It is a WARN for `unavailable`, `rejected`, `outcome_unknown`, and
`integrity`, and stays at DEBUG for `not_found`, `already_exists`,
`too_large`, and `busy`, which answer the caller rather than report a fault.

`aws.request_id` and `aws.extended_request_id` are the store's
`x-amz-request-id` and `x-amz-id-2` response headers, under their
OpenTelemetry names. A provider's support asks for them first, so quote them
from the failure event. They are present once a response arrived, also on a
download that fails later in its body, and after a read retry they name the
last attempt. A failure with no response, such as a timeout, has none. A
value that is not a visible-ASCII token of at most 128 bytes is dropped.

No metric, span, or log from this crate carries a key, bucket, endpoint, URL,
or credential. The SDK itself logs endpoint parameters, which include the key,
at DEBUG and whole requests at TRACE. While this profile is retained, the
process subscriber keeps every `aws_*` target at INFO or quieter, even under
`log.level = "debug"`. A target the directive names, such as
`aws_smithy_runtime=debug`, is exempt with the targets under it; unnamed
siblings stay capped. Name one only to debug the SDK, and only where the keys
may be seen.

## Providers

| | Amazon S3 | Cloudflare R2 | Railway Buckets |
| --- | --- | --- | --- |
| Create-only (`If-None-Match: *`, 412) | documented | documented | Tigris documents it; Railway does not |
| Upload checksum used | CRC64NVME | CRC64NVME, bytes only | none |
| Expected bucket owner | sent on every call; not in presigned URLs, which must work without headers | not supported | not supported |
| Single-upload limit | 5 GB | 4.995 GiB | not documented |
| Throttling | 503 `SlowDown` | 429 on more than one write per second to one key; 503 | not documented |
| Notes | | Jurisdiction buckets answer only on their jurisdiction endpoint | Tigris behind it; public network only; no versioning, object lock, or lifecycle rules; bucket access is suspended when the plan's usage limit is reached |

Without multipart, the client leaves no incomplete uploads, so no lifecycle
rule for abandoned multipart uploads is needed. A bucket's own retention is
the feature's business, for example a scheduled delete of expired keys.

## Local run and proof

`make test` runs the adapter against an in-process HTTP stub. It needs no
credentials and no Docker, and it covers admission, the failure mapping,
one attempt per mutation, read retries, checksum validation, range refusal,
stream length enforcement, an empty download, request identifiers in the
failure event, presign bounds, and redaction.

The emulator proof runs the client against versitygw from
`env/docker-compose.yml`:

```sh
ALLOW_HEAVY=1 make test-integration-object-storage
```

It proves signing, create-only, CRC64NVME in a header and in a trailer,
validation at EOF, presigned expiry, and credential refusal. It does not
certify a provider. For a local service run, start the emulator with
`make compose-up`, create the bucket, and use the `local` provider from
`env/config/local.toml`.

## Live provider conformance

A provider is supported only after a live run against a bucket of that
provider; for `s3_compatible` that is the one store the service uses. The run
uses an access key, so it does not cover `workload_identity`. The run writes to a real bucket, so it is never part of CI and
needs separate authorization for that bucket:

```sh
OBJECT_STORAGE_CONFORMANCE_WRITES=allow make test-object-storage-conformance PROVIDER=railway
```

It reads the service's own `APP__OBJECT_STORAGE__*` variables for that bucket,
writes only under `conformance/<run>/`, and deletes what it wrote. It asserts
the client contract (create-only 412, head, get, delete then not found, the
bucket probe, presigned GET and its expiry) and prints the provider facts the
client does not rely on everywhere: whether a CRC64NVME header and trailer are
accepted, whether a checksum comes back on get, and how an expected-owner
mismatch is answered. Enable a provider feature, such as Railway checksums,
only from such a recorded run. One provider's result never qualifies another.

## Remove the profile

Initialize with `OBJECT_STORAGE=none`. The initializer removes the crate, the
configuration section, the emulator service, the proofs, and this guide
together. Changing the selection after initialization is a refused profile
migration. Do not remove the profile by clearing only `provider`.
<!-- template:end object-storage:docs-object-storage -->
