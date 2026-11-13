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
use operation_context::OperationContext;
use rcgen::{KeyPair, PKCS_RSA_SHA256};
use secrecy::SecretString;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use url::Url;

use crate::{
    AcquisitionError, Algorithm, Credentials, Error, OnBehalfOf, Options, RefreshDriver, Rejection,
    credentials::{Cached, acquisition_deadline},
    options::Admitted,
    token::EVICTION_MIN_AGE,
    token_endpoint::{FETCH_TIMEOUT, TOKEN_LIMITS},
};

// template:begin outbound-auth-grpc:oauth-grpc-tests-module
#[cfg(feature = "grpc")]
mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-tests-module

#[cfg(feature = "integration")]
mod keycloak;

mod admission;
mod exchange;
mod http_client;
mod refresh;
mod service_token;
mod token_request;

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
    token_completed: Semaphore,
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
    driver_shutdown: watch::Sender<()>,
    drivers: Mutex<Vec<JoinHandle<()>>>,
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
            token_completed: Semaphore::new(0),
            token_gate: Mutex::new(None),
            resource_received: Semaphore::new(0),
            resource_gate: Mutex::new(None),
        });
        let (shutdown, receiver) = oneshot::channel();
        let (driver_shutdown, _) = watch::channel(());
        let task = tokio::spawn(serve(listener, state.clone(), receiver));
        Self {
            endpoint,
            origin,
            state,
            shutdown,
            task,
            driver_shutdown,
            drivers: Mutex::default(),
            key: KeyPair::generate_for(&PKCS_RSA_SHA256).unwrap(),
        }
    }

    fn options(&self, scopes: &[&str], audience: Option<&str>) -> Options {
        Options {
            token_url: self.endpoint.to_string(),
            client_id: "client:id".to_owned(),
            private_key: SecretString::from(self.key.serialize_pem()),
            key_id: "key-1".to_owned(),
            algorithm: Algorithm::Rs256,
            assertion_audience: ASSERTION_AUDIENCE.to_owned(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            audience: audience.map(str::to_owned),
            exchange_cache_capacity: 1024,
            provider_concurrency: 32,
        }
    }

    fn credentials(&self, scopes: &[&str], audience: Option<&str>) -> Credentials {
        self.prepare(self.options(scopes, audience))
    }

    /// Admits the fixture's loopback HTTP token endpoint and drives its refresh
    /// lifetime until this fixture finishes.
    fn prepare(&self, options: Options) -> Credentials {
        let (credentials, driver) = self.build(options);
        self.drive(driver);
        credentials
    }

    fn build(&self, options: Options) -> (Credentials, RefreshDriver) {
        let admitted = Admitted::try_new_for_test(options, self.endpoint.clone()).unwrap();
        Credentials::build(
            admitted,
            Client::try_new_for_test_http(&self.origin, TOKEN_LIMITS).unwrap(),
        )
        .unwrap()
    }

    fn drive(&self, driver: RefreshDriver) {
        let mut shutdown = self.driver_shutdown.subscribe();
        self.drivers
            .lock()
            .unwrap()
            .push(tokio::spawn(driver.run(async move {
                let _ = shutdown.changed().await;
            })));
    }

    fn resource_client(&self) -> Client {
        Client::try_new_for_test_http(&self.origin, TOKEN_LIMITS).unwrap()
    }

    fn request(&self) -> Request<Bytes> {
        Request::get(self.origin.join(RESOURCE_PATH).unwrap().as_str())
            .body(Bytes::new())
            .unwrap()
    }

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

    async fn token_completed(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.state.token_completed.acquire())
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
        self.driver_shutdown.send_replace(());
        let drivers = std::mem::take(&mut *self.drivers.lock().unwrap());
        for driver in drivers {
            tokio::time::timeout(Duration::from_secs(2), driver)
                .await
                .unwrap()
                .unwrap();
        }
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
    if is_token {
        state.token_completed.add_permits(1);
    }
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "fixture peer closed before completing headers");
        request.extend_from_slice(&chunk[..read]);
        // Valid token responses may carry a near-1-MiB Bearer value.
        assert!(
            request.len() <= 2 * 1024 * 1024,
            "fixture request exceeded bound"
        );
        if let Some(end) = request.array_windows().position(|part| part == b"\r\n\r\n") {
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

/// A token-exchange response admitted by `token_from_response`'s
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

fn token_context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(10))
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
