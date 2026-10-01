#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt;
use secrecy::SecretString;

use crate::error::{Call, Reply, classify};
use crate::provider::{UploadChecksum, admit};
use crate::{
    ConfigError, ContentType, CredentialSource, ObjectKey, ObjectStorage, ObjectStorageError,
    ObjectStorageOptions, Provider, PutBody, PutOptions,
};

const R2_ENDPOINT: &str = "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";

fn options(provider: Provider) -> ObjectStorageOptions {
    ObjectStorageOptions {
        provider,
        bucket: "evidence-bucket".to_owned(),
        credentials: access_key("AKIDEXAMPLE", "hunter2-secret"),
        max_object_bytes: 1024,
        max_concurrency: 2,
        operation_timeout: Duration::from_secs(2),
    }
}

fn access_key(access_key_id: &str, secret_access_key: &str) -> CredentialSource {
    CredentialSource::AccessKey {
        access_key_id: access_key_id.to_owned(),
        secret_access_key: SecretString::from(secret_access_key.to_owned()),
    }
}

// ---- key and content type -------------------------------------------------

#[test]
fn keys_follow_the_portable_grammar() {
    for key in [
        "a",
        "pricing-evidence/v1/2026/09/29/abc_def.json",
        "parse-results/op-1.json",
        "a.b~c-d_e",
        &"k".repeat(1024),
    ] {
        ObjectKey::new(key).unwrap_or_else(|_| panic!("{key:?} should be admitted"));
    }
    for key in [
        "",
        "/leading",
        "trailing/",
        "double//slash",
        "./dot",
        "a/../b",
        "space here",
        "unicode-é",
        "query?x",
        "percent%20",
        &"k".repeat(1025),
    ] {
        assert!(ObjectKey::new(key).is_err(), "{key:?} should be refused");
    }
}

#[test]
fn key_debug_hides_the_key() {
    let key = ObjectKey::new("customer-42/invoice.pdf").unwrap();
    let rendered = format!("{key:?}");
    assert!(!rendered.contains("customer"), "{rendered}");
}

#[test]
fn content_types_are_header_values() {
    ContentType::new("application/vnd.gonkagate.pricing+json;version=1").unwrap();
    ContentType::new("text/plain; charset=utf-8").unwrap();
    for value in [
        "",
        "   ",
        "text/plain\r\nx: y",
        "tëxt/plain",
        &"a".repeat(1025),
    ] {
        assert!(ContentType::new(value).is_err(), "{value:?}");
    }
}

// ---- provider admission ---------------------------------------------------

#[test]
fn amazon_requires_a_region_and_an_owner() {
    let admitted = admit(
        &Provider::AmazonS3 {
            region: "eu-central-1".to_owned(),
            expected_bucket_owner: "123456789012".to_owned(),
        },
        "evidence-bucket",
    )
    .unwrap();
    assert_eq!(admitted.endpoint, None);
    assert_eq!(
        admitted.expected_bucket_owner.as_deref(),
        Some("123456789012")
    );
    assert_eq!(admitted.checksum, UploadChecksum::Always);
    assert!(!admitted.path_style);

    // Every partition is admitted; its credentials decide what it reaches.
    for region in ["us-gov-west-1", "cn-north-1", "eusc-de-east-1"] {
        let admitted = admit(
            &Provider::AmazonS3 {
                region: region.to_owned(),
                expected_bucket_owner: "123456789012".to_owned(),
            },
            "evidence-bucket",
        );
        assert_eq!(admitted.unwrap().region, region);
    }
    for region in ["", "EU-central-1", "eu central 1", "eu-central-1/"] {
        let refused = admit(
            &Provider::AmazonS3 {
                region: region.to_owned(),
                expected_bucket_owner: "123456789012".to_owned(),
            },
            "evidence-bucket",
        );
        assert_eq!(refused.unwrap_err(), ConfigError::Region, "{region:?}");
    }
    for owner in ["", "12345678901", "1234567890123", "12345678901a"] {
        let refused = admit(
            &Provider::AmazonS3 {
                region: "us-east-1".to_owned(),
                expected_bucket_owner: owner.to_owned(),
            },
            "evidence-bucket",
        );
        assert_eq!(
            refused.unwrap_err(),
            ConfigError::ExpectedBucketOwner,
            "{owner:?}"
        );
    }
}

#[test]
fn r2_admits_only_its_account_origin() {
    for endpoint in [
        R2_ENDPOINT,
        "https://0123456789abcdef0123456789abcdef.eu.r2.cloudflarestorage.com/",
        "https://0123456789abcdef0123456789abcdef.fedramp.r2.cloudflarestorage.com",
    ] {
        let admitted = admit(
            &Provider::CloudflareR2 {
                endpoint: endpoint.to_owned(),
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(admitted.region, "auto");
        assert_eq!(admitted.expected_bucket_owner, None);
        assert_eq!(admitted.checksum, UploadChecksum::BytesOnly);
        assert!(!admitted.endpoint.unwrap().ends_with('/'));
    }
    for endpoint in [
        "http://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/bucket",
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com:8443",
        "https://0123456789abcdef.r2.cloudflarestorage.com",
        "https://0123456789abcdef0123456789abcdef.xx.r2.cloudflarestorage.com",
        "https://evil.example/0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
        "https://user@0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
        "",
    ] {
        let refused = admit(
            &Provider::CloudflareR2 {
                endpoint: endpoint.to_owned(),
            },
            "evidence-bucket",
        );
        assert_eq!(refused.unwrap_err(), ConfigError::Endpoint, "{endpoint:?}");
    }
}

#[test]
fn railway_takes_its_bucket_endpoint_and_defaults_the_region() {
    let admitted = admit(
        &Provider::Railway {
            endpoint: "https://t3.storageapi.dev".to_owned(),
            region: String::new(),
        },
        "evidence-bucket-jdhhd8oe18xi",
    )
    .unwrap();
    assert_eq!(
        admitted.endpoint.as_deref(),
        Some("https://t3.storageapi.dev")
    );
    assert_eq!(admitted.region, "auto");
    assert_eq!(admitted.checksum, UploadChecksum::Never);
    assert!(!admitted.path_style);
    for endpoint in [
        "http://t3.storageapi.dev",
        "https://t3.storageapi.dev/x",
        "t3.storageapi.dev",
    ] {
        let refused = admit(
            &Provider::Railway {
                endpoint: endpoint.to_owned(),
                region: "auto".to_owned(),
            },
            "evidence-bucket",
        );
        assert_eq!(refused.unwrap_err(), ConfigError::Endpoint, "{endpoint:?}");
    }
    let refused = admit(
        &Provider::Railway {
            endpoint: "https://t3.storageapi.dev".to_owned(),
            region: "AMS 1".to_owned(),
        },
        "evidence-bucket",
    );
    assert_eq!(refused.unwrap_err(), ConfigError::Region);
}

#[test]
fn local_allows_plaintext_and_path_style() {
    let admitted = admit(
        &Provider::Local {
            endpoint: "http://127.0.0.1:7070".to_owned(),
            region: String::new(),
        },
        "evidence-bucket",
    )
    .unwrap();
    assert_eq!(admitted.endpoint.as_deref(), Some("http://127.0.0.1:7070"));
    assert_eq!(admitted.region, "us-east-1");
    assert!(admitted.path_style);
}

#[test]
fn a_generic_provider_takes_an_https_origin_and_the_common_subset() {
    let admitted = admit(
        &Provider::S3Compatible {
            endpoint: "https://s3.eu-central-003.backblazeb2.com".to_owned(),
            region: "eu-central-003".to_owned(),
            path_style: false,
        },
        "evidence-bucket",
    )
    .unwrap();
    assert_eq!(
        admitted.endpoint.as_deref(),
        Some("https://s3.eu-central-003.backblazeb2.com")
    );
    assert_eq!(admitted.region, "eu-central-003");
    assert!(!admitted.path_style);
    assert_eq!(admitted.expected_bucket_owner, None);
    assert_eq!(admitted.checksum, UploadChecksum::Never);

    // A self-hosted store: its own port, path-style, and the default region.
    let admitted = admit(
        &Provider::S3Compatible {
            endpoint: "https://ceph.internal.example:8443".to_owned(),
            region: String::new(),
            path_style: true,
        },
        "evidence-bucket",
    )
    .unwrap();
    assert_eq!(
        admitted.endpoint.as_deref(),
        Some("https://ceph.internal.example:8443")
    );
    assert_eq!(admitted.region, "us-east-1");
    assert!(admitted.path_style);

    for endpoint in [
        "http://ceph.internal.example",
        "https://ceph.internal.example/bucket",
        "https://user@ceph.internal.example",
        "",
    ] {
        let refused = admit(
            &Provider::S3Compatible {
                endpoint: endpoint.to_owned(),
                region: String::new(),
                path_style: false,
            },
            "evidence-bucket",
        );
        assert_eq!(refused.unwrap_err(), ConfigError::Endpoint, "{endpoint:?}");
    }
}

#[test]
fn buckets_are_dotless_dns_names() {
    let provider = Provider::Local {
        endpoint: "http://127.0.0.1:7070".to_owned(),
        region: String::new(),
    };
    for bucket in [
        "abc",
        "pricing-evidence",
        "document-processing-results",
        &"a".repeat(63),
    ] {
        admit(&provider, bucket).unwrap_or_else(|_| panic!("{bucket:?}"));
    }
    for bucket in [
        "ab",
        &"a".repeat(64),
        "has.dot",
        "Upper",
        "-leading",
        "trailing-",
    ] {
        assert_eq!(
            admit(&provider, bucket).unwrap_err(),
            ConfigError::Bucket,
            "{bucket:?}"
        );
    }
}

#[test]
fn credentials_are_required_and_redacted() {
    let mut missing_id = options(Provider::Local {
        endpoint: "http://127.0.0.1:1".to_owned(),
        region: String::new(),
    });
    missing_id.credentials = access_key(" ", "hunter2-secret");
    assert_eq!(
        ObjectStorage::new(missing_id).unwrap_err(),
        ConfigError::AccessKeyId
    );

    let mut missing_secret = options(Provider::Local {
        endpoint: "http://127.0.0.1:1".to_owned(),
        region: String::new(),
    });
    missing_secret.credentials = access_key("AKIDEXAMPLE", "");
    assert_eq!(
        ObjectStorage::new(missing_secret).unwrap_err(),
        ConfigError::SecretAccessKey
    );

    let options = options(Provider::CloudflareR2 {
        endpoint: R2_ENDPOINT.to_owned(),
    });
    let rendered_options = format!("{options:?}");
    assert!(!rendered_options.contains("hunter2"), "{rendered_options}");
    let storage = ObjectStorage::new(options).unwrap();
    let rendered = format!("{storage:?}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(!rendered.contains("evidence-bucket"), "{rendered}");
    assert_eq!(storage.provider(), "cloudflare_r2");
}

#[test]
fn workload_identity_is_built_without_io_and_only_for_amazon() {
    let mut amazon = options(Provider::AmazonS3 {
        region: "eu-central-1".to_owned(),
        expected_bucket_owner: "123456789012".to_owned(),
    });
    amazon.credentials = CredentialSource::WorkloadIdentity;
    assert_eq!(format!("{:?}", amazon.credentials), "WorkloadIdentity");
    // No runtime and no network: credentials load on the first call.
    assert_eq!(ObjectStorage::new(amazon).unwrap().provider(), "amazon_s3");

    for provider in [
        Provider::CloudflareR2 {
            endpoint: R2_ENDPOINT.to_owned(),
        },
        Provider::S3Compatible {
            endpoint: "https://ceph.internal.example".to_owned(),
            region: String::new(),
            path_style: true,
        },
        Provider::Local {
            endpoint: "http://127.0.0.1:1".to_owned(),
            region: String::new(),
        },
    ] {
        let mut other = options(provider);
        other.credentials = CredentialSource::WorkloadIdentity;
        assert_eq!(
            ObjectStorage::new(other).unwrap_err(),
            ConfigError::Credentials
        );
    }
}

#[test]
fn a_concurrency_limit_outside_the_semaphore_range_is_refused() {
    for max_concurrency in [0, usize::MAX] {
        let mut refused = options(Provider::Local {
            endpoint: "http://127.0.0.1:1".to_owned(),
            region: String::new(),
        });
        refused.max_concurrency = max_concurrency;
        assert_eq!(
            ObjectStorage::new(refused).unwrap_err(),
            ConfigError::MaxConcurrency
        );
    }
}

// ---- failure classification -----------------------------------------------

#[test]
fn classification_separates_not_applied_from_unknown() {
    use ObjectStorageError as E;
    let status = |status, code| Reply::Status { status, code };
    let cases = [
        (Call::Read, Reply::Lost, E::Unavailable),
        (Call::Mutation, Reply::Lost, E::OutcomeUnknown),
        (Call::CreateOnly, Reply::Lost, E::OutcomeUnknown),
        (Call::Mutation, Reply::NotSent, E::Rejected),
        (Call::Read, status(404, Some("NoSuchKey")), E::NotFound),
        (Call::Read, status(404, Some("NotFound")), E::NotFound),
        (Call::Read, status(404, None), E::NotFound),
        (Call::Read, status(404, Some("NoSuchBucket")), E::Rejected),
        (
            Call::Mutation,
            status(404, Some("NoSuchBucket")),
            E::Rejected,
        ),
        (
            Call::CreateOnly,
            status(412, Some("PreconditionFailed")),
            E::AlreadyExists,
        ),
        (
            Call::Mutation,
            status(412, Some("PreconditionFailed")),
            E::Rejected,
        ),
        (
            Call::CreateOnly,
            status(409, Some("ConditionalRequestConflict")),
            E::Unavailable,
        ),
        (
            Call::Mutation,
            status(429, Some("TooManyRequests")),
            E::Unavailable,
        ),
        (
            Call::Mutation,
            status(503, Some("SlowDown")),
            E::Unavailable,
        ),
        (
            Call::Mutation,
            status(500, Some("InternalError")),
            E::OutcomeUnknown,
        ),
        (Call::CreateOnly, status(502, None), E::OutcomeUnknown),
        (
            Call::Read,
            status(500, Some("InternalError")),
            E::Unavailable,
        ),
        (Call::Read, status(403, Some("AccessDenied")), E::Rejected),
        (Call::Mutation, status(400, Some("BadDigest")), E::Rejected),
        (
            Call::Mutation,
            status(403, Some("SignatureDoesNotMatch")),
            E::Rejected,
        ),
        (
            Call::Mutation,
            status(400, Some("RequestTimeout")),
            E::Unavailable,
        ),
        (
            Call::Mutation,
            status(501, Some("NotImplemented")),
            E::Rejected,
        ),
        (Call::Read, status(501, Some("NotImplemented")), E::Rejected),
    ];
    for (call, reply, expected) in cases {
        assert_eq!(classify(call, reply), expected, "{call:?} {reply:?}");
    }
}

#[test]
fn errors_display_no_provider_detail() {
    for error in [
        ObjectStorageError::NotFound,
        ObjectStorageError::OutcomeUnknown,
        ObjectStorageError::Integrity,
    ] {
        let rendered = error.to_string();
        assert!(rendered.starts_with("object"), "{rendered}");
    }
}

// ---- adapter over an in-process HTTP stub ---------------------------------

/// One request the stub saw: method, path, and headers. Bodies are drained.
#[derive(Clone, Debug)]
struct Seen {
    method: Method,
    path: String,
    headers: HeaderMap,
}

type Respond = Arc<dyn Fn(&Seen, usize) -> Response + Send + Sync>;

/// A plaintext S3 stand-in on 127.0.0.1. `respond` gets each request and its
/// zero-based index.
struct Stub {
    endpoint: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Stub {
    async fn start(respond: impl Fn(&Seen, usize) -> Response + Send + Sync + 'static) -> Self {
        Self::start_delaying(|_| Duration::ZERO, respond).await
    }

    async fn start_with_delay(
        response_delay: Duration,
        respond: impl Fn(&Seen, usize) -> Response + Send + Sync + 'static,
    ) -> Self {
        Self::start_delaying(move |_| response_delay, respond).await
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "a test-local S3 stand-in, not an application route"
    )]
    /// `delay_of` gets a request's zero-based index and holds its response.
    async fn start_delaying(
        delay_of: impl Fn(usize) -> Duration + Send + Sync + 'static,
        respond: impl Fn(&Seen, usize) -> Response + Send + Sync + 'static,
    ) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let delay_of = Arc::new(delay_of);
        let respond: Respond = Arc::new(respond);
        let counter = Arc::new(AtomicUsize::new(0));
        let router = axum::Router::new().route(
            "/{*path}",
            axum::routing::any({
                let seen = Arc::clone(&seen);
                move |request: Request| {
                    let seen = Arc::clone(&seen);
                    let respond = Arc::clone(&respond);
                    let counter = Arc::clone(&counter);
                    let delay_of = Arc::clone(&delay_of);
                    async move {
                        let (parts, body) = request.into_parts();
                        let _ = body.collect().await;
                        let record = Seen {
                            method: parts.method,
                            path: parts.uri.path().to_owned(),
                            headers: parts.headers,
                        };
                        let index = counter.fetch_add(1, Ordering::SeqCst);
                        let response = respond(&record, index);
                        seen.lock().unwrap().push(record);
                        let response_delay = delay_of(index);
                        if !response_delay.is_zero() {
                            tokio::time::sleep(response_delay).await;
                        }
                        response
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            endpoint,
            seen,
            shutdown,
            task,
        }
    }

    fn storage(&self, configure: impl FnOnce(&mut ObjectStorageOptions)) -> ObjectStorage {
        let mut options = options(Provider::Local {
            endpoint: self.endpoint.clone(),
            region: String::new(),
        });
        configure(&mut options);
        ObjectStorage::new(options).unwrap()
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    async fn stop(self) {
        self.shutdown.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

fn xml_error(status: StatusCode, code: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/xml")
        .body(Body::from(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>stub</Message><RequestId>stub</RequestId></Error>"
        )))
        .unwrap()
}

fn ok_empty() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("etag", "\"stub\"")
        .body(Body::empty())
        .unwrap()
}

fn object(body: &'static [u8], extra: &[(&str, &str)]) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header("content-length", body.len().to_string())
        .header("last-modified", "Tue, 29 Sep 2026 12:00:00 GMT")
        .header("etag", "\"stub\"");
    for (name, value) in extra {
        response = response.header(*name, *value);
    }
    response.body(Body::from(body)).unwrap()
}

fn key() -> ObjectKey {
    ObjectKey::new("results/op-1.json").unwrap()
}

#[tokio::test]
async fn create_only_put_sends_if_none_match_once_and_maps_412() {
    let stub =
        Stub::start(|_, _| xml_error(StatusCode::PRECONDITION_FAILED, "PreconditionFailed")).await;
    let storage = stub.storage(|_| {});
    let result = storage
        .put(
            &key(),
            Bytes::from_static(b"{}").into(),
            PutOptions::default()
                .content_type(ContentType::new("application/json").unwrap())
                .create_only(),
        )
        .await;
    assert_eq!(result, Err(ObjectStorageError::AlreadyExists));
    let seen = stub.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, Method::PUT);
    assert_eq!(seen[0].path, "/evidence-bucket/results/op-1.json");
    assert_eq!(seen[0].headers["if-none-match"], "*");
    assert_eq!(seen[0].headers["content-type"], "application/json");
    assert!(seen[0].headers.contains_key("x-amz-checksum-crc64nvme"));
    assert!(!seen[0].headers.contains_key("x-amz-expected-bucket-owner"));
}

#[tokio::test]
async fn a_provider_without_proven_checksums_gets_no_checksum_headers() {
    let stub = Stub::start(|_, _| ok_empty()).await;
    // Railway's admission refuses plaintext, so build its policy on the stub.
    let mut storage = stub.storage(|_| {});
    std::sync::Arc::get_mut(&mut storage.inner)
        .unwrap()
        .checksum = UploadChecksum::Never;
    storage
        .put(
            &key(),
            Bytes::from_static(b"{}").into(),
            PutOptions::default(),
        )
        .await
        .unwrap();
    let seen = stub.seen();
    let names: Vec<&str> = seen[0]
        .headers
        .keys()
        .map(axum::http::HeaderName::as_str)
        .collect();
    assert!(
        !names.iter().any(|name| name.starts_with("x-amz-checksum")
            || *name == "x-amz-sdk-checksum-algorithm"
            || *name == "x-amz-trailer"),
        "{names:?}"
    );
}

#[tokio::test]
async fn failed_create_only_put_is_not_retried_and_is_unknown() {
    let stub =
        Stub::start(|_, _| xml_error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError")).await;
    let storage = stub.storage(|_| {});
    let result = storage
        .put(
            &key(),
            Bytes::from_static(b"{}").into(),
            PutOptions::default().create_only(),
        )
        .await;
    assert_eq!(result, Err(ObjectStorageError::OutcomeUnknown));
    assert_eq!(stub.seen().len(), 1);
}

#[tokio::test]
async fn mutations_make_one_attempt() {
    // A retry would leave only the last reply, which could hide an earlier
    // attempt that applied the mutation.
    let stub = Stub::start(|_, _| xml_error(StatusCode::SERVICE_UNAVAILABLE, "SlowDown")).await;
    let storage = stub.storage(|_| {});
    let result = storage
        .put(
            &key(),
            Bytes::from_static(b"{}").into(),
            PutOptions::default(),
        )
        .await;
    assert_eq!(result, Err(ObjectStorageError::Unavailable));
    assert_eq!(stub.seen().len(), 1);

    let stub =
        Stub::start(|_, _| xml_error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError")).await;
    let storage = stub.storage(|_| {});
    assert_eq!(
        storage.delete(&key()).await,
        Err(ObjectStorageError::OutcomeUnknown)
    );
    assert_eq!(stub.seen().len(), 1);
}

#[tokio::test]
async fn a_stream_that_differs_from_its_length_is_rejected() {
    for (declared, sent) in [(4_u64, &b"longer"[..]), (16, &b"short"[..]), (0, &b"x"[..])] {
        let stub = Stub::start(|_, _| ok_empty()).await;
        let storage = stub.storage(|_| {});
        let body = http_body_util::Full::new(Bytes::from_static(sent));
        let result = storage
            .put(
                &key(),
                PutBody::stream(declared, body),
                PutOptions::default(),
            )
            .await;
        assert_eq!(
            result,
            Err(ObjectStorageError::Rejected),
            "declared {declared}"
        );
    }
}

#[tokio::test]
async fn streamed_put_declares_its_length_and_is_sent_once() {
    let stub =
        Stub::start(|_, _| xml_error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError")).await;
    let storage = stub.storage(|_| {});
    // A request body: axum's `Body` is `Send` but not `Sync`.
    let body = Body::from("streamed");
    let result = storage
        .put(&key(), PutBody::stream(8, body), PutOptions::default())
        .await;
    assert_eq!(result, Err(ObjectStorageError::OutcomeUnknown));
    assert_eq!(stub.seen().len(), 1);
}

#[tokio::test]
async fn empty_stream_preparation_and_response_share_one_put_timeout() {
    // Each phase fits alone; together they exceed the two-second call budget.
    // Delays model slow input/provider I/O, rather than synchronize test tasks.
    struct DelayedEof(std::pin::Pin<Box<tokio::time::Sleep>>);

    impl http_body::Body for DelayedEof {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            std::future::Future::poll(self.0.as_mut(), context).map(|()| None)
        }
    }

    let delay = Duration::from_millis(1500);
    let stub = Stub::start_with_delay(delay, |_, _| ok_empty()).await;
    let storage = stub.storage(|options| options.max_concurrency = 1);
    let body = DelayedEof(Box::pin(tokio::time::sleep(delay)));
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        storage.put(&key(), PutBody::stream(0, body), PutOptions::default()),
    )
    .await
    .unwrap();
    let sent = stub.seen().len();
    // A timed-out call releases its slot, even though the provider may finish.
    let follow_up = storage
        .put(&key(), Bytes::new().into(), PutOptions::default())
        .await;
    stub.stop().await;
    assert_eq!(result, Err(ObjectStorageError::OutcomeUnknown));
    assert_eq!(sent, 1, "the mutation must be dispatched exactly once");
    assert_eq!(follow_up, Ok(()));
}

#[tokio::test]
async fn oversized_put_sends_nothing() {
    let stub = Stub::start(|_, _| ok_empty()).await;
    let storage = stub.storage(|options| options.max_object_bytes = 4);
    let result = storage
        .put(
            &key(),
            Bytes::from_static(b"12345").into(),
            PutOptions::default(),
        )
        .await;
    assert_eq!(result, Err(ObjectStorageError::TooLarge));
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn get_reads_the_body_and_validates_a_returned_checksum() {
    // CRC64NVME of b"{\"ok\":true}".
    let checksum = {
        use aws_smithy_checksums::ChecksumAlgorithm;
        let mut checksum = ChecksumAlgorithm::Crc64Nvme.into_impl();
        checksum.update(b"{\"ok\":true}");
        let headers = checksum.headers();
        headers["x-amz-checksum-crc64nvme"]
            .to_str()
            .unwrap()
            .to_owned()
    };
    let checksum: &'static str = Box::leak(checksum.into_boxed_str());
    let stub = Stub::start(move |_, _| {
        object(
            b"{\"ok\":true}",
            &[
                ("x-amz-checksum-crc64nvme", checksum),
                ("x-amz-checksum-type", "FULL_OBJECT"),
            ],
        )
    })
    .await;
    let storage = stub.storage(|_| {});
    let download = storage.get(&key()).await.unwrap();
    assert_eq!(download.metadata().size, 11);
    assert_eq!(
        download.metadata().content_type.as_deref(),
        Some("application/json")
    );
    assert!(download.metadata().last_modified.is_some());
    assert_eq!(
        download.bytes().await.unwrap(),
        Bytes::from_static(b"{\"ok\":true}")
    );
    assert_eq!(stub.seen()[0].headers["x-amz-checksum-mode"], "ENABLED");

    // The download is itself a body of exactly the object's size.
    let download = storage.get(&key()).await.unwrap();
    assert_eq!(http_body::Body::size_hint(&download).exact(), Some(11));
    assert!(!http_body::Body::is_end_stream(&download));
    assert_eq!(
        download.collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"{\"ok\":true}")
    );
}

#[tokio::test]
async fn get_with_a_wrong_checksum_fails_integrity_at_the_end() {
    let stub = Stub::start(|_, _| {
        object(
            b"{\"ok\":true}",
            &[
                ("x-amz-checksum-crc64nvme", "AAAAAAAAAAA="),
                ("x-amz-checksum-type", "FULL_OBJECT"),
            ],
        )
    })
    .await;
    let storage = stub.storage(|_| {});
    let mut download = storage.get(&key()).await.unwrap();
    let mut outcome = download.next_chunk().await;
    while let Ok(Some(_)) = outcome {
        outcome = download.next_chunk().await;
    }
    assert_eq!(outcome, Err(ObjectStorageError::Integrity));
    // A later call must not read as a clean end.
    assert_eq!(
        download.next_chunk().await,
        Err(ObjectStorageError::Integrity)
    );

    // A reader that stops at the declared length, as hyper does for a
    // response body, never receives the bytes that complete the object.
    let mut download = storage.get(&key()).await.unwrap();
    let mut received = 0;
    let failure = loop {
        match download.frame().await {
            Some(Ok(frame)) => received += frame.into_data().unwrap().len(),
            Some(Err(error)) => break error,
            None => panic!("a failed check must not read as a clean end"),
        }
    };
    assert_eq!(failure, ObjectStorageError::Integrity);
    assert!(received < 11, "{received}");
}

#[tokio::test]
async fn get_without_a_checksum_is_readable() {
    let stub = Stub::start(|_, _| object(b"legacy", &[])).await;
    let storage = stub.storage(|_| {});
    let body = storage.get(&key()).await.unwrap().bytes().await.unwrap();
    assert_eq!(body, Bytes::from_static(b"legacy"));

    let mut download = storage.get(&key()).await.unwrap();
    let first = download.next_chunk().await.unwrap().unwrap();
    assert_eq!(
        download.bytes().await.unwrap(),
        Bytes::from_static(b"legacy").slice(first.len()..)
    );
}

#[tokio::test]
async fn an_empty_object_ends_inside_get() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let stub = Stub::start(|_, _| object(b"", &[])).await;
    let storage = stub.storage(|options| options.max_concurrency = 1);
    let download = storage.get(&key()).await.unwrap();
    // An HTTP server never polls a body that is already at its end, so the
    // outcome and the slot must not wait for a reader.
    assert!(http_body::Body::is_end_stream(&download));
    assert_eq!(http_body::Body::size_hint(&download).exact(), Some(0));
    let rendered = handle.render();
    assert!(
        rendered.contains(
            r#"object_storage_operation_duration_seconds_count{operation="get",outcome="ok"} 1"#
        ),
        "{rendered}"
    );
    storage.head(&key()).await.unwrap();
    assert_eq!(download.bytes().await.unwrap(), Bytes::new());
    assert!(!handle.render().contains("cancelled"));
}

#[tokio::test]
async fn get_refuses_ranges_and_oversized_objects() {
    let stub = Stub::start(|_, _| object(b"partial", &[("content-range", "bytes 0-6/100")])).await;
    let storage = stub.storage(|_| {});
    assert_eq!(
        storage.get(&key()).await.err(),
        Some(ObjectStorageError::Integrity)
    );

    let stub = Stub::start(|_, _| object(b"12345", &[])).await;
    let storage = stub.storage(|options| options.max_object_bytes = 4);
    assert_eq!(
        storage.get(&key()).await.err(),
        Some(ObjectStorageError::TooLarge)
    );
}

#[tokio::test]
async fn missing_objects_are_not_found_and_reads_retry() {
    let stub = Stub::start(|_, _| xml_error(StatusCode::NOT_FOUND, "NoSuchKey")).await;
    let storage = stub.storage(|_| {});
    assert_eq!(
        storage.get(&key()).await.err(),
        Some(ObjectStorageError::NotFound)
    );
    assert_eq!(stub.seen().len(), 1);

    let stub = Stub::start(|_, _| xml_error(StatusCode::SERVICE_UNAVAILABLE, "SlowDown")).await;
    // Two jittered backoffs of at most 1 s each fit this budget.
    let storage = stub.storage(|options| options.operation_timeout = Duration::from_secs(10));
    assert_eq!(
        storage.get(&key()).await.err(),
        Some(ObjectStorageError::Unavailable)
    );
    assert_eq!(stub.seen().len(), 3);
}

#[tokio::test]
async fn a_hung_read_attempt_leaves_room_for_a_retry() {
    // The first response never arrives inside the 4 s budget. The attempt
    // bound (half of it) gives up on it, and the retry is answered at once.
    let stub = Stub::start_delaying(
        |index| Duration::from_secs(if index == 0 { 30 } else { 0 }),
        |_, _| object(b"{}", &[]),
    )
    .await;
    let storage = stub.storage(|options| options.operation_timeout = Duration::from_secs(4));
    assert_eq!(
        storage.head(&key()).await.map(|metadata| metadata.size),
        Ok(2)
    );
    assert_eq!(stub.seen().len(), 2);
}

#[tokio::test]
async fn a_mutation_keeps_the_whole_budget_for_its_one_attempt() {
    // Slower than a read attempt's share of the 2 s budget, but inside it.
    let stub = Stub::start_with_delay(Duration::from_millis(1300), |_, _| ok_empty()).await;
    let storage = stub.storage(|_| {});
    let result = storage
        .put(
            &key(),
            Bytes::from_static(b"{}").into(),
            PutOptions::default(),
        )
        .await;
    assert_eq!(result, Ok(()));
    assert_eq!(storage.delete(&key()).await, Ok(()));
    assert_eq!(stub.seen().len(), 2);
}

#[tokio::test]
async fn a_failed_credential_load_sends_nothing_and_is_retryable() {
    #[derive(Debug)]
    struct NoIdentity;

    impl aws_sdk_s3::config::ProvideCredentials for NoIdentity {
        fn provide_credentials<'a>(
            &'a self,
        ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            aws_credential_types::provider::future::ProvideCredentials::ready(Err(
                aws_credential_types::provider::error::CredentialsError::provider_error(
                    "the identity endpoint is unreachable",
                ),
            ))
        }
    }

    let stub = Stub::start(|_, _| ok_empty()).await;
    let options = options(Provider::Local {
        endpoint: stub.endpoint.clone(),
        region: String::new(),
    });
    let admitted = admit(&options.provider, &options.bucket).unwrap();
    let storage = ObjectStorage::build(
        options,
        admitted,
        crate::https_client(),
        aws_sdk_s3::config::SharedCredentialsProvider::new(NoIdentity),
    );
    // Nothing was sent, so even a mutation's outcome is known.
    let put = storage
        .put(
            &key(),
            Bytes::from_static(b"{}").into(),
            PutOptions::default().create_only(),
        )
        .await;
    assert_eq!(put, Err(ObjectStorageError::Unavailable));
    assert_eq!(
        storage.head(&key()).await,
        Err(ObjectStorageError::Unavailable)
    );
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn head_reports_size_above_the_limit() {
    let stub = Stub::start(|_, _| {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-length", "4096")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap()
    })
    .await;
    let storage = stub.storage(|options| options.max_object_bytes = 4);
    let metadata = storage.head(&key()).await.unwrap();
    assert_eq!(metadata.size, 4096);

    let stub = Stub::start(|_, _| {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;
    let storage = stub.storage(|_| {});
    assert_eq!(
        storage.head(&key()).await,
        Err(ObjectStorageError::NotFound)
    );
}

#[tokio::test]
async fn delete_of_a_missing_key_succeeds_and_a_missing_bucket_is_rejected() {
    let stub = Stub::start(|_, _| {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap()
    })
    .await;
    stub.storage(|_| {}).delete(&key()).await.unwrap();

    let stub = Stub::start(|_, _| xml_error(StatusCode::NOT_FOUND, "NoSuchKey")).await;
    stub.storage(|_| {}).delete(&key()).await.unwrap();

    let stub = Stub::start(|_, _| xml_error(StatusCode::NOT_FOUND, "NoSuchBucket")).await;
    assert_eq!(
        stub.storage(|_| {}).delete(&key()).await,
        Err(ObjectStorageError::Rejected)
    );
}

#[tokio::test]
async fn admission_refuses_excess_and_a_download_holds_its_slot() {
    let stub = Stub::start(|_, _| object(b"held", &[])).await;
    let storage = stub.storage(|options| options.max_concurrency = 1);
    let held = storage.get(&key()).await.unwrap();
    assert_eq!(storage.head(&key()).await, Err(ObjectStorageError::Busy));
    assert_eq!(stub.seen().len(), 1);
    drop(held);
    let mut completed = storage.get(&key()).await.unwrap();
    while completed.next_chunk().await.unwrap().is_some() {}
    assert_eq!(completed.next_chunk().await.unwrap(), None);
    // A completed download releases admission even while the value is retained.
    storage.head(&key()).await.unwrap();
}

#[tokio::test]
async fn unreachable_endpoint_is_unavailable_for_reads_and_unknown_for_writes() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut options = options(Provider::Local {
        endpoint,
        region: String::new(),
    });
    options.operation_timeout = Duration::from_secs(5);
    let storage = ObjectStorage::new(options).unwrap();
    assert_eq!(
        storage.head(&key()).await,
        Err(ObjectStorageError::Unavailable)
    );
    assert_eq!(
        storage
            .put(
                &key(),
                Bytes::from_static(b"x").into(),
                PutOptions::default()
            )
            .await,
        Err(ObjectStorageError::OutcomeUnknown)
    );
}

#[tokio::test]
async fn presign_is_bounded_and_redacted() {
    let stub = Stub::start(|_, _| ok_empty()).await;
    let mut storage = stub.storage(|_| {});
    // As on Amazon S3: the owner header would have to travel with the URL.
    std::sync::Arc::get_mut(&mut storage.inner)
        .unwrap()
        .expected_bucket_owner = Some("123456789012".to_owned());
    let url = storage
        .presign_get(&key(), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(url.expose().contains("X-Amz-Signature="), "presigned query");
    assert!(url.expose().contains("X-Amz-Expires=60"));
    assert!(
        url.expose().contains("X-Amz-SignedHeaders=host&"),
        "{}",
        url.expose()
    );
    assert_eq!(format!("{url:?}"), "PresignedUrl([REDACTED])");
    for expires_in in [Duration::ZERO, Duration::from_secs(7 * 24 * 60 * 60 + 1)] {
        assert_eq!(
            storage.presign_get(&key(), expires_in).await.err(),
            Some(ObjectStorageError::Rejected)
        );
    }
    assert!(stub.seen().is_empty(), "presigning sends nothing");
}

/// Every span and event a test emits, one line each with its recorded fields.
#[derive(Clone, Default)]
struct Records(Arc<Mutex<Vec<String>>>);

struct Line(String);

impl tracing::field::Visit for Line {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let _ = write!(self.0, " {}={value}", field.name());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl Records {
    fn push(&self, line: Line) {
        self.0.lock().unwrap().push(line.0);
    }

    fn matching(&self, kind: &str, field: &str) -> Vec<String> {
        let lines = self.0.lock().unwrap();
        lines
            .iter()
            .filter(|line| line.starts_with(kind) && line.contains(field))
            .cloned()
            .collect()
    }
}

impl tracing::Subscriber for Records {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut line = Line("span".to_owned());
        span.record(&mut line);
        self.push(line);
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut line = Line("span".to_owned());
        values.record(&mut line);
        self.push(line);
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut line = Line("event".to_owned());
        event.record(&mut line);
        self.push(line);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn a_failure_reports_the_request_identifiers_of_its_response() {
    const REQUEST_ID: &str = "4442587FB7D0A2F9";
    const EXTENDED_REQUEST_ID: &str =
        "eftixk72aD6Ap51TnqcoF8eFidJG9Z/2mkiDFu8yU9AS1ed4OpIszj7UDNEHGran";
    let stub = Stub::start(|_, index| {
        let mut response = xml_error(StatusCode::FORBIDDEN, "AccessDenied");
        let headers = response.headers_mut();
        if index == 0 {
            headers.insert("x-amz-request-id", REQUEST_ID.parse().unwrap());
            headers.insert("x-amz-id-2", EXTENDED_REQUEST_ID.parse().unwrap());
        } else {
            // Not an identifier: a store may send any header text.
            headers.insert("x-amz-request-id", "not an identifier".parse().unwrap());
        }
        response
    })
    .await;
    let storage = stub.storage(|_| {});
    let records = Records::default();
    let _subscriber = tracing::subscriber::set_default(records.clone());
    // tracing caches each callsite's interest for the whole process. While
    // one dispatcher exists it asks the registering thread's default, so
    // another test's thread could cache "never" for the callsites read here.
    // A second live dispatcher makes tracing consult every dispatcher.
    let _every_dispatcher = tracing::Dispatch::new(Records::default());

    assert_eq!(
        storage.head(&key()).await,
        Err(ObjectStorageError::Rejected)
    );
    let failed = records.matching("event", "object_storage_operation_failed");
    let [event] = failed.as_slice() else {
        panic!("one failure event: {failed:?}");
    };
    assert!(event.contains(" error.type=403"), "{event}");
    assert!(
        event.contains(&format!(" aws.request_id={REQUEST_ID}")),
        "{event}"
    );
    assert!(
        event.contains(&format!(" aws.extended_request_id={EXTENDED_REQUEST_ID}")),
        "{event}"
    );
    assert_eq!(
        records
            .matching("span", &format!(" aws.request_id={REQUEST_ID}"))
            .len(),
        1,
        "the span carries the identifier"
    );
    assert!(!event.contains("results/op-1.json"), "{event}");

    assert_eq!(
        storage.head(&key()).await,
        Err(ObjectStorageError::Rejected)
    );
    let failed = records.matching("event", "object_storage_operation_failed");
    assert_eq!(failed.len(), 2);
    assert!(!failed[1].contains("request_id"), "{}", failed[1]);
}

#[tokio::test]
async fn probe_reports_only_the_error_type() {
    use health::Probe;
    let stub = Stub::start(|_, _| xml_error(StatusCode::FORBIDDEN, "AccessDenied")).await;
    let probe = stub.storage(|_| {}).probe();
    assert_eq!(probe.name(), "object_storage");
    let error = probe.check().await.unwrap_err().to_string();
    assert_eq!(error, "object storage bucket check failed: 403");
}

#[tokio::test]
async fn metrics_carry_only_operation_and_outcome() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let stub = Stub::start(|_, _| xml_error(StatusCode::NOT_FOUND, "NoSuchKey")).await;
    let storage = stub.storage(|_| {});
    metrics::with_local_recorder(&recorder, || {
        // Handles bind on first use, so resolve them under this recorder.
        let guard = storage.start(crate::observe::Operation::Head);
        drop(guard);
        storage.start(crate::observe::Operation::Head).succeed();
        storage
            .start(crate::observe::Operation::Head)
            .fail(ObjectStorageError::NotFound, "404");
        storage
            .start(crate::observe::Operation::Head)
            .fail(ObjectStorageError::Integrity, "content_length");
    });
    let rendered = handle.render();
    assert!(
        rendered.contains(r#"object_storage_operation_duration_seconds_count{operation="head",outcome="cancelled"} 1"#),
        "{rendered}"
    );
    assert!(!rendered.contains("results/op-1.json"));
    for outcome in ["ok", "not_found", "integrity"] {
        assert!(
            rendered.contains(&format!(
                "object_storage_operation_duration_seconds_count{{operation=\"head\",outcome=\"{outcome}\"}} 1"
            )),
            "{rendered}"
        );
    }
}
