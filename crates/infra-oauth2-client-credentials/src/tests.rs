#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "bounded local HTTP fixtures make setup failures test failures"
)]

use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

use bytes::Bytes;
use http::{Request, StatusCode, header};
use infra_outbound_http::Client;
use rcgen::{KeyPair, PKCS_RSA_SHA256};
use secrecy::SecretString;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, oneshot},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use url::Url;

use super::{
    AcquisitionError, Algorithm, Cached, Credentials, EVICTION_MIN_AGE, Error, FETCH_TIMEOUT,
    OnBehalfOf, Options, Rejection, TOKEN_LIMITS, subject_key,
};

// template:begin outbound-auth-grpc:oauth-grpc-tests-module
#[cfg(feature = "grpc")]
mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-tests-module

#[cfg(feature = "integration")]
mod keycloak;

const TOKEN_PATH: &str = "/token";
const RESOURCE_PATH: &str = "/resource";
const ASSERTION_AUDIENCE: &str = "https://issuer.example";
/// Independent of the crate's own constants, so a typo in one does not hide
/// behind an identical typo in the other.
const GRANT_TYPE_CLIENT_CREDENTIALS: &str = "client_credentials";
const GRANT_TYPE_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

#[derive(Clone, Debug)]
struct CapturedRequest {
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

struct FixtureState {
    token_response: Mutex<Vec<u8>>,
    resource_response: Mutex<Vec<u8>>,
    token_requests: Mutex<Vec<CapturedRequest>>,
    resource_requests: Mutex<Vec<CapturedRequest>>,
    token_received: Semaphore,
    token_gate: Mutex<Option<Arc<Semaphore>>>,
    resource_received: Semaphore,
    resource_gate: Mutex<Option<Arc<Semaphore>>>,
}

struct Fixture {
    endpoint: Url,
    origin: Url,
    state: Arc<FixtureState>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    key: KeyPair,
}

impl Fixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = Url::parse(&format!("http://127.0.0.1:{}", address.port())).unwrap();
        let endpoint = origin.join(TOKEN_PATH).unwrap();
        let state = Arc::new(FixtureState {
            token_response: Mutex::new(json_response(
                "200 OK",
                &serde_json::json!({
                    "access_token": "fixture-token",
                    "token_type": "Bearer",
                    "expires_in": 60,
                }),
            )),
            resource_response: Mutex::new(response("200 OK", b"ok")),
            token_requests: Mutex::new(Vec::new()),
            resource_requests: Mutex::new(Vec::new()),
            token_received: Semaphore::new(0),
            token_gate: Mutex::new(None),
            resource_received: Semaphore::new(0),
            resource_gate: Mutex::new(None),
        });
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve(listener, state.clone(), receiver));
        Self {
            endpoint,
            origin,
            state,
            shutdown,
            task,
            key: KeyPair::generate_for(&PKCS_RSA_SHA256).unwrap(),
        }
    }

    fn credentials(&self, scopes: &[&str], audience: Option<&str>) -> Credentials {
        let options = Options {
            token_url: self.endpoint.to_string(),
            client_id: "client:id".to_owned(),
            private_key: SecretString::from(self.key.serialize_pem()),
            key_id: "key-1".to_owned(),
            algorithm: Algorithm::Rs256,
            assertion_audience: ASSERTION_AUDIENCE.to_owned(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            audience: audience.map(str::to_owned),
        };
        Credentials::prepare(
            options,
            &self.endpoint,
            Client::new_for_test_http(&self.origin, TOKEN_LIMITS).unwrap(),
        )
        .unwrap()
    }

    fn resource_client(&self) -> Client {
        Client::new_for_test_http(&self.origin, TOKEN_LIMITS).unwrap()
    }

    fn request(&self) -> Request<Bytes> {
        Request::get(self.origin.join(RESOURCE_PATH).unwrap().as_str())
            .body(Bytes::new())
            .unwrap()
    }

    /// A resource request acting on behalf of `subject`.
    fn on_behalf_of_request(&self, subject: &str) -> Request<Bytes> {
        let mut request = self.request();
        request
            .extensions_mut()
            .insert(OnBehalfOf::new(SecretString::from(subject)));
        request
    }

    fn token_json(&self, status: &str, body: &serde_json::Value) {
        *self.state.token_response.lock().unwrap() = json_response(status, body);
    }

    fn token_raw(&self, response: Vec<u8>) {
        *self.state.token_response.lock().unwrap() = response;
    }

    fn resource_status(&self, status: &str) {
        *self.state.resource_response.lock().unwrap() = response(status, b"resource");
    }

    fn block_tokens(&self) -> Arc<Semaphore> {
        self.state
            .token_received
            .forget_permits(self.state.token_received.available_permits());
        let gate = Arc::new(Semaphore::new(0));
        *self.state.token_gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    async fn token_received(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.state.token_received.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }

    fn block_resources(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.state.resource_gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    async fn resource_received(&self) {
        tokio::time::timeout(
            Duration::from_secs(2),
            self.state.resource_received.acquire(),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
    }

    fn token_requests(&self) -> Vec<CapturedRequest> {
        self.state.token_requests.lock().unwrap().clone()
    }

    fn resource_requests(&self) -> Vec<CapturedRequest> {
        self.state.resource_requests.lock().unwrap().clone()
    }

    async fn finish(self) {
        self.shutdown.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn serve(
    listener: TcpListener,
    state: Arc<FixtureState>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => result.unwrap(),
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                connections.spawn(handle(stream, state.clone()));
            }
        }
    }
    connections.abort_all();
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            assert!(
                error.is_cancelled(),
                "fixture connection failed before shutdown: {error}"
            );
        }
    }
}

async fn handle(mut stream: TcpStream, state: Arc<FixtureState>) {
    let request = read_request(&mut stream).await;
    let is_token = request.target == TOKEN_PATH;
    let response = if is_token {
        state.token_requests.lock().unwrap().push(request);
        state.token_received.add_permits(1);
        let gate = state.token_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.acquire().await.unwrap().forget();
        }
        state.token_response.lock().unwrap().clone()
    } else {
        state.resource_requests.lock().unwrap().push(request);
        state.resource_received.add_permits(1);
        let gate = state.resource_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.acquire().await.unwrap().forget();
        }
        state.resource_response.lock().unwrap().clone()
    };
    let _ = stream.write_all(&response).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "fixture peer closed before completing headers");
        request.extend_from_slice(&chunk[..read]);
        assert!(
            request.len() <= 128 * 1024,
            "fixture request exceeded bound"
        );
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = std::str::from_utf8(&request[..header_end]).unwrap();
    let mut lines = head.split("\r\n");
    let target = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let body_bytes = headers
        .get("content-length")
        .map_or(0, |value| value.parse::<usize>().unwrap());
    while request.len() < header_end + body_bytes {
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "fixture peer closed before completing body");
        request.extend_from_slice(&chunk[..read]);
    }
    CapturedRequest {
        target,
        headers,
        body: request[header_end..header_end + body_bytes].to_vec(),
    }
}

fn response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn json_response(status: &str, body: &serde_json::Value) -> Vec<u8> {
    response(status, &serde_json::to_vec(body).unwrap())
}

/// A token-exchange response admitted by [`super::into_token`]'s
/// `issued_token_type` check.
fn exchange_response(access_token: &str) -> serde_json::Value {
    serde_json::json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 60,
        "issued_token_type": ACCESS_TOKEN_TYPE,
    })
}

fn deadline(after: Duration) -> Instant {
    Instant::now() + after
}

/// Moves the Tokio clock without waiting, leaving it running for socket I/O.
async fn advance(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

async fn poll_pending<T>(mut future: Pin<&mut impl Future<Output = T>>) {
    assert!(
        poll_fn(|context| Poll::Ready(future.as_mut().poll(context)))
            .await
            .is_pending(),
        "future completed before its fixture event"
    );
}

/// Order-preserving decode of a `x-www-form-urlencoded` body.
fn form_pairs(body: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

fn form_value<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .unwrap_or_else(|| panic!("missing form field {key}"))
        .1
        .as_str()
}

#[derive(serde::Deserialize)]
struct DecodedAssertion {
    iss: String,
    sub: String,
    aud: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    jti: String,
}

/// Decodes and verifies a captured request's `client_assertion` field with
/// the fixture's matching public key.
fn decode_assertion(
    fixture: &Fixture,
    request: &CapturedRequest,
) -> (jsonwebtoken::Header, DecodedAssertion) {
    let pairs = form_pairs(&request.body);
    let assertion = form_value(&pairs, "client_assertion");
    let header = jsonwebtoken::decode_header(assertion).unwrap();
    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.set_audience(&[ASSERTION_AUDIENCE]);
    let key =
        jsonwebtoken::DecodingKey::from_rsa_pem(fixture.key.public_key_pem().as_bytes()).unwrap();
    let data = jsonwebtoken::decode::<DecodedAssertion>(assertion, &key, &validation).unwrap();
    (header, data.claims)
}

fn valid_options() -> Options {
    Options {
        token_url: "https://identity.example/token".to_owned(),
        client_id: "client".to_owned(),
        private_key: SecretString::from("placeholder"),
        key_id: "key-1".to_owned(),
        algorithm: Algorithm::default(),
        assertion_audience: "https://issuer.example".to_owned(),
        scopes: Vec::new(),
        audience: None,
    }
}

#[test]
fn direct_construction_repeats_sensitive_option_admission() {
    for (options, key) in [
        (
            Options {
                client_id: String::new(),
                ..valid_options()
            },
            "client_id",
        ),
        (
            Options {
                key_id: String::new(),
                ..valid_options()
            },
            "key_id",
        ),
        (
            Options {
                assertion_audience: String::new(),
                ..valid_options()
            },
            "assertion_audience",
        ),
        (
            Options {
                private_key: SecretString::from(""),
                ..valid_options()
            },
            "private_key",
        ),
        (
            Options {
                scopes: vec!["bad scope".to_owned()],
                ..valid_options()
            },
            "scopes",
        ),
        (
            Options {
                token_url: "https://user:secret@identity.example/token".to_owned(),
                ..valid_options()
            },
            "token_url",
        ),
        (
            Options {
                audience: Some(String::new()),
                ..valid_options()
            },
            "audience",
        ),
    ] {
        let mut options = options;
        let error = Credentials::new(options.clone()).unwrap_err();
        assert_eq!(error.key, key);
        options.private_key = SecretString::from("different-secret");
        assert!(!format!("{options:?} {error:?}").contains("different-secret"));
    }
}

#[test]
fn a_key_mismatched_with_its_algorithm_or_not_pem_is_refused_at_construction() {
    let ec_pem = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .unwrap()
        .serialize_pem();
    for private_key in [ec_pem, "not a pem key".to_owned()] {
        let options = Options {
            private_key: SecretString::from(private_key),
            algorithm: Algorithm::Rs256,
            ..valid_options()
        };
        let error = Credentials::new(options).unwrap_err();
        assert_eq!(error.key, "private_key");
    }
}

#[tokio::test]
async fn client_credentials_request_has_no_authorization_header_and_expected_fields() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({
            "access_token": "token+/_~.=",
            "token_type": "bEaReR",
            "expires_in": 60,
            "refresh_token": "private-refresh",
        }),
    );
    let response = fixture
        .credentials(&["read", "write"], Some("https://api.example/resource"))
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = fixture.token_requests().pop().unwrap();
    assert_eq!(token.target, TOKEN_PATH);
    assert!(token.header("authorization").is_none());
    let pairs = form_pairs(&token.body);
    let keys = pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "grant_type",
            "client_id",
            "client_assertion_type",
            "client_assertion",
            "scope",
            "audience",
        ]
    );
    assert_eq!(
        form_value(&pairs, "grant_type"),
        GRANT_TYPE_CLIENT_CREDENTIALS
    );
    assert_eq!(form_value(&pairs, "client_id"), "client:id");
    assert_eq!(
        form_value(&pairs, "client_assertion_type"),
        CLIENT_ASSERTION_TYPE
    );
    assert!(!form_value(&pairs, "client_assertion").is_empty());
    assert_eq!(form_value(&pairs, "scope"), "read write");
    assert_eq!(
        form_value(&pairs, "audience"),
        "https://api.example/resource"
    );
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer token+/_~.=")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn optional_scope_and_audience_are_omitted_from_the_client_credentials_form() {
    let fixture = Fixture::new().await;
    fixture
        .credentials(&[], None)
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    let pairs = form_pairs(&fixture.token_requests()[0].body);
    let keys = pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "grant_type",
            "client_id",
            "client_assertion_type",
            "client_assertion"
        ]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn assertion_header_and_claims_are_verifiable_and_two_requests_differ_in_jti() {
    let fixture = Fixture::new().await;
    for _ in 0..2 {
        fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    let requests = fixture.token_requests();
    assert_eq!(requests.len(), 2);
    let decoded = requests
        .iter()
        .map(|request| decode_assertion(&fixture, request))
        .collect::<Vec<_>>();
    for (header, claims) in &decoded {
        assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("key-1"));
        assert_eq!(header.typ.as_deref(), Some("client-authentication+jwt"));
        assert_eq!(claims.iss, "client:id");
        assert_eq!(claims.sub, "client:id");
        assert_eq!(claims.aud, ASSERTION_AUDIENCE);
        assert_eq!(claims.exp - claims.iat, 60);
        assert_eq!(claims.iat, claims.nbf);
        // Dated ten seconds back for a provider whose clock is behind.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!((10..=15).contains(&(now - claims.iat)));
        assert!(!claims.jti.is_empty());
    }
    assert_ne!(decoded[0].1.jti, decoded[1].1.jti);
    fixture.finish().await;
}

#[tokio::test]
async fn token_exchange_request_has_expected_form_fields() {
    let fixture = Fixture::new().await;
    fixture.token_json("200 OK", &exchange_response("exchanged"));
    fixture
        .credentials(&["read"], Some("https://api.example/resource"))
        .http(fixture.resource_client())
        .execute(
            fixture.on_behalf_of_request("subject-token"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let token = fixture.token_requests().pop().unwrap();
    assert!(token.header("authorization").is_none());
    let pairs = form_pairs(&token.body);
    let keys = pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "grant_type",
            "subject_token",
            "subject_token_type",
            "requested_token_type",
            "scope",
            "audience",
            "client_id",
            "client_assertion_type",
            "client_assertion",
        ]
    );
    assert_eq!(form_value(&pairs, "grant_type"), GRANT_TYPE_TOKEN_EXCHANGE);
    assert_eq!(form_value(&pairs, "subject_token"), "subject-token");
    assert_eq!(form_value(&pairs, "subject_token_type"), ACCESS_TOKEN_TYPE);
    assert_eq!(
        form_value(&pairs, "requested_token_type"),
        ACCESS_TOKEN_TYPE
    );
    assert_eq!(form_value(&pairs, "scope"), "read");
    assert_eq!(
        form_value(&pairs, "audience"),
        "https://api.example/resource"
    );
    assert_eq!(form_value(&pairs, "client_id"), "client:id");
    assert_eq!(
        form_value(&pairs, "client_assertion_type"),
        CLIENT_ASSERTION_TYPE
    );
    assert!(!form_value(&pairs, "client_assertion").is_empty());
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer exchanged")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn exchange_without_the_access_token_issued_type_is_refused() {
    let fixture = Fixture::new().await;
    for issued_token_type in [None, Some("urn:ietf:params:oauth:token-type:jwt")] {
        let mut body = serde_json::json!({
            "access_token": "x",
            "token_type": "Bearer",
            "expires_in": 60,
        });
        if let Some(value) = issued_token_type {
            body["issued_token_type"] = value.into();
        }
        fixture.token_json("200 OK", &body);
        let result = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(
                fixture.on_behalf_of_request("subject"),
                deadline(Duration::from_secs(10)),
            )
            .await;
        assert!(matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        ));
    }
    fixture.finish().await;
}

#[tokio::test]
async fn an_exchanged_token_is_reused_per_subject_but_not_across_subjects() {
    let fixture = Fixture::new().await;
    fixture.token_json("200 OK", &exchange_response("first"));
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(
            fixture.on_behalf_of_request("alice"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    client
        .execute(
            fixture.on_behalf_of_request("alice"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    fixture.token_json("200 OK", &exchange_response("second"));
    client
        .execute(
            fixture.on_behalf_of_request("bob"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    let authorizations = fixture
        .resource_requests()
        .into_iter()
        .map(|request| request.header("authorization").unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        authorizations,
        ["Bearer first", "Bearer first", "Bearer second"]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_short_lived_exchanged_token_serves_its_own_request_after_one_exchange() {
    let fixture = Fixture::new().await;
    let mut short = exchange_response("short");
    short["expires_in"] = serde_json::json!(5);
    fixture.token_json("200 OK", &short);
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    for expected_exchanges in 1..=2 {
        client
            .execute(
                fixture.on_behalf_of_request("alice"),
                deadline(Duration::from_secs(10)),
            )
            .await
            .unwrap();
        // Inside its reuse margin, it is never reused, nor fetched twice.
        assert_eq!(fixture.token_requests().len(), expected_exchanges);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn concurrent_on_behalf_of_calls_for_one_subject_share_one_exchange() {
    let fixture = Fixture::new().await;
    fixture.token_json("200 OK", &exchange_response("shared"));
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(
        fixture.on_behalf_of_request("subject"),
        deadline(Duration::from_secs(10)),
    ));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut waiter = Box::pin(client.execute(
        fixture.on_behalf_of_request("subject"),
        deadline(Duration::from_secs(10)),
    ));
    poll_pending(waiter.as_mut()).await;
    assert_eq!(fixture.token_requests().len(), 1);
    gate.add_permits(2);
    let (leader, waiter) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(leader, waiter)
    })
    .await
    .unwrap();
    leader.unwrap();
    waiter.unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_resource_401_evicts_only_that_subjects_exchanged_token() {
    let fixture = Fixture::new().await;
    fixture.token_json("200 OK", &exchange_response("shared"));
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(
            fixture.on_behalf_of_request("alice"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    client
        .execute(
            fixture.on_behalf_of_request("bob"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status("401 Unauthorized");
    client
        .execute(
            fixture.on_behalf_of_request("alice"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    fixture.resource_status("200 OK");
    let before = fixture.token_requests().len();
    client
        .execute(
            fixture.on_behalf_of_request("alice"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), before + 1);
    client
        .execute(
            fixture.on_behalf_of_request("bob"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), before + 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_failed_exchange_is_not_cached() {
    let fixture = Fixture::new().await;
    fixture.token_raw(response("500 Internal Server Error", b"{}"));
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    for _ in 0..2 {
        let result = client
            .execute(
                fixture.on_behalf_of_request("subject"),
                deadline(Duration::from_secs(10)),
            )
            .await;
        assert!(matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::Unavailable))
        ));
    }
    assert_eq!(fixture.token_requests().len(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn tokens_that_cannot_form_a_header_are_refused_before_resource_dispatch() {
    let fixture = Fixture::new().await;
    for token in ["", "control\u{7f}character"] {
        fixture.token_json(
            "200 OK",
            &serde_json::json!({"access_token": token, "token_type": "BEARER", "expires_in": 60}),
        );
        let result = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await;
        assert!(matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        ));
    }
    assert_eq!(fixture.token_requests().len(), 2);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn short_lived_tokens_are_not_reused_but_tokens_without_expiry_are_until_a_401() {
    let fixture = Fixture::new().await;
    for (body, fetches_for_two_calls) in [
        (
            serde_json::json!({"access_token": "short", "token_type": "Bearer", "expires_in": 10}),
            2,
        ),
        (
            serde_json::json!({"access_token": "no-expiry", "token_type": "Bearer"}),
            1,
        ),
        (
            serde_json::json!({"access_token": "overflow", "token_type": "Bearer", "expires_in": u64::MAX}),
            1,
        ),
    ] {
        fixture.token_json("200 OK", &body);
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let before = fixture.token_requests().len();
        for _ in 0..2 {
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
        }
        assert_eq!(
            fixture.token_requests().len(),
            before + fetches_for_two_calls
        );
    }

    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status("401 Unauthorized");
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.resource_status("200 OK");
    let before = fixture.token_requests().len();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), before + 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_cached_token_is_refreshed_after_its_reuse_cutoff() {
    let fixture = Fixture::new().await;
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    for _ in 0..2 {
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    assert_eq!(fixture.token_requests().len(), 1);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(51)).await;
    tokio::time::resume();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(fixture.resource_requests().len(), 3);
    fixture.finish().await;
}

#[tokio::test]
async fn near_its_cutoff_a_token_is_replaced_in_the_background_while_callers_reuse_it() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    // Just inside the five minutes before the 3590 s reuse cutoff.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3291)).await;
    tokio::time::resume();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 3600}),
    );
    let gate = fixture.block_tokens();
    for _ in 0..2 {
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    fixture.token_received().await;
    assert_eq!(fixture.token_requests().len(), 2);
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
            let requests = fixture.resource_requests();
            if requests.last().unwrap().header("authorization") == Some("Bearer second") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    let authorizations = fixture
        .resource_requests()
        .iter()
        .map(|request| request.header("authorization").unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        authorizations[..3]
            .iter()
            .all(|value| value == "Bearer first")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_failed_background_refresh_keeps_the_token_and_retries_after_a_pause() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "kept", "token_type": "Bearer", "expires_in": 3600}),
    );
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_raw(response("503 Service Unavailable", b"{}"));
    let gate = fixture.block_tokens();
    gate.add_permits(Semaphore::MAX_PERMITS / 2);
    for (advance, token_requests) in [(3291, 2), (0, 2), (31, 3)] {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(advance)).await;
        tokio::time::resume();
        let before = fixture.token_requests().len();
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
        if token_requests > before {
            fixture.token_received().await;
        }
        assert_eq!(fixture.token_requests().len(), token_requests);
    }
    assert!(
        fixture
            .resource_requests()
            .iter()
            .all(|request| request.header("authorization") == Some("Bearer kept"))
    );
    fixture.finish().await;
}

#[tokio::test]
async fn zero_or_expired_during_acquisition_never_authorizes_dispatch() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token":"expired", "token_type":"Bearer", "expires_in":0}),
    );
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    assert!(matches!(
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    ));
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token":"late", "token_type":"Bearer", "expires_in":1}),
    );
    let gate = fixture.block_tokens();
    let mut operation =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut operation => panic!("response must be gated: {result:?}"), }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    gate.add_permits(1);
    assert!(matches!(
        operation.await,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    ));
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn clones_reuse_one_owner_cache_but_independent_owners_do_not_share() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let credentials = fixture.credentials(&[], None);
    credentials
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    credentials
        .clone()
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    fixture
        .credentials(&[], None)
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    let authorizations = fixture
        .resource_requests()
        .into_iter()
        .map(|request| request.header("authorization").unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        authorizations,
        ["Bearer first", "Bearer first", "Bearer second"]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_waiter_shares_a_reusable_token_but_retries_a_failure_after_the_leader() {
    let fixture = Fixture::new().await;
    for (body, expected_error, waiter_fetches) in [
        (
            &serde_json::json!({"access_token": "retained", "token_type": "Bearer", "expires_in": 60}),
            None,
            0,
        ),
        (
            &serde_json::json!({"access_token": "short", "token_type": "Bearer", "expires_in": 10}),
            None,
            1,
        ),
        (
            &serde_json::json!({"error": "provider-secret"}),
            Some(AcquisitionError::InvalidResponse),
            1,
        ),
    ] {
        fixture.token_json("200 OK", body);
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let gate = fixture.block_tokens();
        let before_tokens = fixture.token_requests().len();
        let mut leader =
            Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
        tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut waiter =
            Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
        poll_pending(waiter.as_mut()).await;
        // The waiter queues behind the leader instead of sending its own request.
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        gate.add_permits(2);
        let (leader, waiter) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(leader, waiter)
        })
        .await
        .unwrap();
        for result in [leader, waiter] {
            match expected_error {
                Some(expected) => {
                    assert!(matches!(result, Err(Error::Acquisition(error)) if error == expected));
                }
                None => assert!(result.is_ok()),
            }
        }
        assert_eq!(
            fixture.token_requests().len(),
            before_tokens + 1 + waiter_fetches
        );
        *fixture.state.token_gate.lock().unwrap() = None;
    }
    fixture.finish().await;
}

#[tokio::test]
async fn cancelling_service_token_acquisition_allows_a_waiter_to_retry() {
    let fixture = Fixture::new().await;
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut survivor =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    poll_pending(survivor.as_mut()).await;
    drop(leader);
    tokio::select! { () = fixture.token_received() => {}, result = &mut survivor => panic!("replacement must be gated: {result:?}"), }
    gate.add_permits(2);
    survivor.await.unwrap();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn each_waiter_keeps_its_own_deadline_without_cancelling_the_leader() {
    let fixture = Fixture::new().await;
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut short_waiter =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(1))));
    poll_pending(short_waiter.as_mut()).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        short_waiter.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    ));
    tokio::time::resume();
    gate.add_permits(1);
    leader.await.unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    assert_eq!(fixture.resource_requests().len(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn token_wait_spends_the_original_resource_deadline_before_dispatch() {
    let fixture = Fixture::new().await;
    let gate = fixture.block_tokens();
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let mut exchange =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(1))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut exchange => panic!("response must be gated: {result:?}"), }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        exchange.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    ));
    tokio::time::resume();
    drop(gate);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn token_failures_are_sanitized_and_never_dispatch_the_resource() {
    let fixture = Fixture::new().await;
    for (response, expected) in [
        (
            b"HTTP/1.1 302 Found\r\nlocation: http://redirected.example/token\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::Rejected(Rejection::Other),
        ),
        (
            response("503 Service Unavailable", b"provider-secret-body"),
            AcquisitionError::Unavailable,
        ),
        (
            response("429 Too Many Requests", b"provider-secret-body"),
            AcquisitionError::Unavailable,
        ),
        (
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1048577\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::ResponseLimit,
        ),
        (
            response(
                "400 Bad Request",
                br#"{"error":"invalid_client","error_description":"provider-secret-body"}"#,
            ),
            AcquisitionError::Rejected(Rejection::InvalidClient),
        ),
        (
            response(
                "400 Bad Request",
                br#"{"error":"provider-secret-body"}"#,
            ),
            AcquisitionError::Rejected(Rejection::Other),
        ),
    ] {
        fixture.token_raw(response);
        let error = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap_err();
        assert!(matches!(&error, Error::Acquisition(actual) if *actual == expected));
        assert!(!format!("{error:?}").contains("provider-secret-body"));
        assert!(!error.to_string().contains("provider-secret-body"));
    }
    let gate = fixture.block_tokens();
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let mut timed = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut timed => panic!("response must be gated: {result:?}"), }
    tokio::time::pause();
    tokio::time::advance(FETCH_TIMEOUT).await;
    assert!(matches!(
        timed.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    ));
    tokio::time::resume();
    drop(gate);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn caller_authorization_conflict_refuses_before_token_or_resource_io() {
    let fixture = Fixture::new().await;
    for mut conflicting in [fixture.request(), fixture.on_behalf_of_request("subject")] {
        conflicting.headers_mut().insert(
            header::AUTHORIZATION,
            "Bearer caller-token".parse().unwrap(),
        );
        assert!(matches!(
            fixture
                .credentials(&[], None)
                .http(fixture.resource_client())
                .execute(conflicting, deadline(Duration::from_secs(10)))
                .await,
            Err(Error::AuthorizationConflict)
        ));
    }
    assert!(fixture.token_requests().is_empty());
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn resource_401_and_403_pass_through_and_only_401_evicts_the_token() {
    let fixture = Fixture::new().await;
    for (status, follow_up_fetches) in [("401 Unauthorized", 1), ("403 Forbidden", 0)] {
        fixture.resource_status("200 OK");
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let before_tokens = fixture.token_requests().len();
        let before_resources = fixture.resource_requests().len();
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
        advance(EVICTION_MIN_AGE).await;
        fixture.resource_status(status);
        assert_eq!(
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap()
                .status()
                .as_u16(),
            status[..3].parse::<u16>().unwrap()
        );
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        assert_eq!(fixture.resource_requests().len(), before_resources + 2);
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
        assert_eq!(
            fixture.token_requests().len(),
            before_tokens + 1 + follow_up_fetches
        );
        assert_eq!(fixture.resource_requests().len(), before_resources + 3);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn a_resource_that_refuses_every_token_costs_one_token_request_per_eviction_age() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "refused", "token_type": "Bearer", "expires_in": 3600}),
    );
    fixture.resource_status("401 Unauthorized");
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    for token_requests in [1, 2] {
        // Every call is refused, yet a token younger than the eviction age
        // is kept instead of being requested again.
        for _ in 0..3 {
            let response = client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(fixture.token_requests().len(), token_requests);
        }
        advance(EVICTION_MIN_AGE).await;
        // Old enough now: this 401 evicts it for the next round.
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    fixture.finish().await;
}

#[tokio::test]
async fn a_late_401_does_not_evict_a_newer_token() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let credentials = fixture.credentials(&[], None);
    let client = credentials.http(fixture.resource_client());
    // Old enough that only its replacement keeps the late 401 from evicting.
    credentials
        .service_token(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status("401 Unauthorized");
    let gate = fixture.block_resources();
    let mut stale = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.resource_received() => {}, result = &mut stale => panic!("response must be gated: {result:?}"), }

    *credentials.cached() = Cached::default();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    credentials
        .service_token(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();

    gate.add_permits(1);
    assert_eq!(stale.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    *fixture.state.resource_gate.lock().unwrap() = None;
    fixture.resource_status("200 OK");
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(
        fixture
            .resource_requests()
            .last()
            .unwrap()
            .header("authorization"),
        Some("Bearer second")
    );
    fixture.finish().await;
}
