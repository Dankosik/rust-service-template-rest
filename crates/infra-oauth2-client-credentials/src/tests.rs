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
use infra_outbound_http::{Client, Operation};
use secrecy::SecretString;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, oneshot},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use url::Url;

use super::{AcquisitionError, Credentials, Error, FETCH_TIMEOUT, Options, TOKEN_LIMITS};

// template:begin outbound-auth-grpc:oauth-grpc-tests-module
#[cfg(feature = "grpc")]
mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-tests-module

const TOKEN_PATH: &str = "/token";
const RESOURCE_PATH: &str = "/resource";

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
}

struct Fixture {
    endpoint: Url,
    origin: String,
    state: Arc<FixtureState>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://127.0.0.1:{}", address.port());
        let endpoint = Url::parse(&format!("{origin}{TOKEN_PATH}")).unwrap();
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
        });
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve(listener, state.clone(), receiver));
        Self {
            endpoint,
            origin,
            state,
            shutdown,
            task,
        }
    }

    fn credentials(&self, scopes: &[&str], audience: Option<&str>) -> Credentials {
        let options = Options {
            token_url: self.endpoint.to_string(),
            client_id: "client:id".to_owned(),
            client_secret: SecretString::from("secret value"),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            audience: audience.map(str::to_owned),
        };
        Credentials::prepare(
            options,
            self.endpoint.clone(),
            Client::new_for_test_http(&self.origin, TOKEN_LIMITS).unwrap(),
        )
        .unwrap()
    }

    fn resource_client(&self) -> Client {
        Client::new_for_test_http(
            &self.origin,
            infra_outbound_http::Limits {
                max_active: 16,
                ..TOKEN_LIMITS
            },
        )
        .unwrap()
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

fn request() -> Request<Bytes> {
    Request::get(RESOURCE_PATH).body(Bytes::new()).unwrap()
}

fn operation(after: Duration) -> Operation {
    Operation {
        deadline: Instant::now() + after,
        response_body_bytes: None,
    }
}

async fn poll_pending<T>(mut future: Pin<&mut impl Future<Output = T>>) {
    assert!(
        poll_fn(|context| Poll::Ready(future.as_mut().poll(context)))
            .await
            .is_pending(),
        "future completed before its fixture event"
    );
}

#[test]
fn direct_construction_repeats_sensitive_option_admission() {
    for (mut options, key) in [
        (
            Options {
                token_url: "https://identity.example/token".to_owned(),
                client_id: String::new(),
                client_secret: SecretString::from("secret"),
                scopes: Vec::new(),
                audience: None,
            },
            "client_id",
        ),
        (
            Options {
                token_url: "https://identity.example/token".to_owned(),
                client_id: "client".to_owned(),
                client_secret: SecretString::from(""),
                scopes: Vec::new(),
                audience: None,
            },
            "client_secret",
        ),
        (
            Options {
                token_url: "https://identity.example/token".to_owned(),
                client_id: "client".to_owned(),
                client_secret: SecretString::from("secret"),
                scopes: vec!["bad scope".to_owned()],
                audience: None,
            },
            "scopes",
        ),
        (
            Options {
                token_url: "https://user:secret@identity.example/token".to_owned(),
                client_id: "client".to_owned(),
                client_secret: SecretString::from("secret"),
                scopes: Vec::new(),
                audience: None,
            },
            "token_url",
        ),
        (
            Options {
                token_url: "https://identity.example/token".to_owned(),
                client_id: "client".to_owned(),
                client_secret: SecretString::from("secret"),
                scopes: Vec::new(),
                audience: Some(String::new()),
            },
            "audience",
        ),
    ] {
        let error = Credentials::new(options.clone()).unwrap_err();
        assert_eq!(error.key, key);
        options.client_secret = SecretString::from("different-secret");
        assert!(!format!("{options:?} {error:?}").contains("different-secret"));
    }
}

#[tokio::test]
async fn token_exchange_encodes_basic_scopes_and_audience_then_injects_bearer() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({
            "access_token": "token+/_~.=",
            "token_type": "bEaReR",
            "expires_in": 60,
            "provider_extension": { "nested": ["ignored"] },
            "refresh_token": "private-refresh",
        }),
    );
    let response = fixture
        .credentials(&["read", "write"], Some("https://api.example/resource"))
        .http(fixture.resource_client())
        .execute(request(), operation(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = fixture.token_requests().pop().unwrap();
    assert_eq!(token.target, TOKEN_PATH);
    assert_eq!(
        token.header("authorization"),
        Some("Basic Y2xpZW50JTNBaWQ6c2VjcmV0K3ZhbHVl")
    );
    assert_eq!(
        std::str::from_utf8(&token.body).unwrap(),
        "grant_type=client_credentials&scope=read+write&audience=https%3A%2F%2Fapi.example%2Fresource"
    );
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer token+/_~.=")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn optional_scope_and_audience_are_omitted_from_the_form() {
    let fixture = Fixture::new().await;
    fixture
        .credentials(&[], None)
        .http(fixture.resource_client())
        .execute(request(), operation(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(&fixture.token_requests()[0].body).unwrap(),
        "grant_type=client_credentials"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn bearer_grammar_refuses_unsafe_tokens_before_resource_dispatch() {
    let fixture = Fixture::new().await;
    for token in ["", "contains space", "token=middle=padding"] {
        fixture.token_json(
            "200 OK",
            &serde_json::json!({"access_token": token, "token_type": "BEARER", "expires_in": 60}),
        );
        let result = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(request(), operation(Duration::from_secs(10)))
            .await;
        assert!(matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        ));
    }
    assert_eq!(fixture.token_requests().len(), 3);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn short_lived_and_missing_expiry_tokens_are_not_retained() {
    let fixture = Fixture::new().await;
    for body in [
        &serde_json::json!({"access_token": "short", "token_type": "Bearer", "expires_in": 10}),
        &serde_json::json!({"access_token": "no-expiry", "token_type": "Bearer"}),
        &serde_json::json!({"access_token": "overflow", "token_type": "Bearer", "expires_in": u64::MAX}),
    ] {
        fixture.token_json("200 OK", body);
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        client
            .execute(request(), operation(Duration::from_secs(10)))
            .await
            .unwrap();
        client
            .execute(request(), operation(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    assert_eq!(fixture.token_requests().len(), 6);
    assert_eq!(fixture.resource_requests().len(), 6);
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
            .execute(request(), operation(Duration::from_secs(10)))
            .await,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    ));
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token":"late", "token_type":"Bearer", "expires_in":1}),
    );
    let gate = fixture.block_tokens();
    let mut operation = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
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
        .execute(request(), operation(Duration::from_secs(10)))
        .await
        .unwrap();
    credentials
        .clone()
        .http(fixture.resource_client())
        .execute(request(), operation(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    fixture
        .credentials(&[], None)
        .http(fixture.resource_client())
        .execute(request(), operation(Duration::from_secs(10)))
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
async fn concurrent_callers_coalesce_success_failure_and_nonretained_results() {
    let fixture = Fixture::new().await;
    for (body, expected_error, retained) in [
        (
            &serde_json::json!({"access_token": "retained", "token_type": "Bearer", "expires_in": 60}),
            None,
            true,
        ),
        (
            &serde_json::json!({"access_token": "short", "token_type": "Bearer", "expires_in": 10}),
            None,
            false,
        ),
        (
            &serde_json::json!({"error": "provider-secret"}),
            Some(AcquisitionError::InvalidResponse),
            false,
        ),
    ] {
        fixture.token_json("200 OK", body);
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let gate = fixture.block_tokens();
        let before_tokens = fixture.token_requests().len();
        let before_resources = fixture.resource_requests().len();
        let mut leader = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
        tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut waiter = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
        poll_pending(waiter.as_mut()).await;
        gate.add_permits(1);
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
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        assert_eq!(
            fixture.resource_requests().len(),
            before_resources + usize::from(expected_error.is_none()) * 2
        );
        *fixture.state.token_gate.lock().unwrap() = None;
        let follow_up = client
            .execute(request(), operation(Duration::from_secs(10)))
            .await;
        match expected_error {
            Some(expected) => {
                assert!(matches!(follow_up, Err(Error::Acquisition(error)) if error == expected));
            }
            None => assert!(follow_up.is_ok()),
        }
        assert_eq!(
            fixture.token_requests().len(),
            before_tokens + if retained { 1 } else { 2 }
        );
        *fixture.state.token_gate.lock().unwrap() = None;
    }
    fixture.finish().await;
}

#[tokio::test]
async fn cancelling_the_initiator_allows_a_waiter_to_replace_the_token_exchange() {
    let fixture = Fixture::new().await;
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut survivor = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
    poll_pending(survivor.as_mut()).await;
    drop(leader);
    tokio::select! { () = fixture.token_received() => {}, result = &mut survivor => panic!("replacement must be gated: {result:?}"), }
    gate.add_permits(2);
    survivor.await.unwrap();
    client
        .execute(request(), operation(Duration::from_secs(10)))
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
    let mut leader = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut short_waiter = Box::pin(client.execute(request(), operation(Duration::from_secs(1))));
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
    let mut exchange = Box::pin(client.execute(request(), operation(Duration::from_secs(1))));
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
            AcquisitionError::Rejected,
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
            AcquisitionError::Rejected,
        ),
    ] {
        fixture.token_raw(response);
        let error = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(request(), operation(Duration::from_secs(10)))
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
    let mut timed = Box::pin(client.execute(request(), operation(Duration::from_secs(10))));
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
    let mut conflicting = request();
    conflicting.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer caller-token".parse().unwrap(),
    );
    assert!(matches!(
        fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(conflicting, operation(Duration::from_secs(10)))
            .await,
        Err(Error::AuthorizationConflict)
    ));
    assert!(fixture.token_requests().is_empty());
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn resource_401_and_403_pass_through_without_token_replay() {
    let fixture = Fixture::new().await;
    for status in ["401 Unauthorized", "403 Forbidden"] {
        fixture.resource_status(status);
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let before_tokens = fixture.token_requests().len();
        let before_resources = fixture.resource_requests().len();
        assert_eq!(
            client
                .execute(request(), operation(Duration::from_secs(10)))
                .await
                .unwrap()
                .status()
                .as_u16(),
            status[..3].parse::<u16>().unwrap()
        );
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        assert_eq!(fixture.resource_requests().len(), before_resources + 1);
        client
            .execute(request(), operation(Duration::from_secs(10)))
            .await
            .unwrap();
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        assert_eq!(fixture.resource_requests().len(), before_resources + 2);
    }
    fixture.finish().await;
}
