#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "bounded local HTTP fixtures make setup failures test failures"
)]

use std::{
    collections::BTreeMap,
    future::poll_fn,
    num::NonZeroU32,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
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
    AcquisitionError, Algorithm, EmptySubject, Error, ExchangeHttpClient, ExchangeTokenTarget,
    ExchangeTokens, IssuerUrl, OnBehalfOf, Options, PrivateKeyJwt, RefreshDriver, Registration,
    Rejection, ServiceHttpClient, ServiceTokenTarget, ServiceTokens, TokenUrl,
    credentials::acquisition_deadline,
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
mod discovery;
mod exchange;
mod http_client;
mod refresh;
mod service_token;
mod token_request;

const TOKEN_PATH: &str = "/token";
const RESOURCE_PATH: &str = "/resource";
const ASSERTION_AUDIENCE: &str = "https://issuer.example";
const GRANT_TYPE_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const EXCHANGE_CAPACITY: NonZeroU32 = NonZeroU32::new(1024).unwrap();

fn nonzero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

fn subject(value: &str) -> OnBehalfOf {
    OnBehalfOf::try_new(SecretString::from(value)).unwrap()
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
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
    registrations: Mutex<Vec<Registration>>,
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
                StatusCode::OK,
                &serde_json::json!({
                    "access_token": "fixture-token",
                    "token_type": "Bearer",
                    "expires_in": 60,
                }),
            )),
            resource_response: Mutex::new(response(StatusCode::OK, b"ok")),
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
            registrations: Mutex::default(),
            key: KeyPair::generate_for(&PKCS_RSA_SHA256).unwrap(),
        }
    }

    fn options(&self) -> Options {
        Options {
            token_url: TokenUrl::parse_for_test_http(self.endpoint.as_str()).unwrap(),
            client_id: "client:id".to_owned(),
            auth: PrivateKeyJwt {
                private_key: SecretString::from(self.key.serialize_pem()),
                key_id: "key-1".to_owned(),
                algorithm: Algorithm::Rs256,
                audience: IssuerUrl::parse(ASSERTION_AUDIENCE).unwrap(),
            },
            provider_concurrency: nonzero(32),
        }
    }

    fn resource_origin(&self) -> http::Uri {
        self.origin.as_str().parse().unwrap()
    }

    fn service_tokens(&self, scopes: &[&str]) -> ServiceTokens {
        self.service_tokens_with(self.options(), scopes, self.resource_origin())
    }

    fn service_tokens_with(
        &self,
        options: Options,
        scopes: &[&str],
        origin: http::Uri,
    ) -> ServiceTokens {
        let registration = prepare_registration(options);
        let (tokens, driver) = registration.service_tokens(service_target(scopes), origin);
        self.drive(driver);
        self.registrations.lock().unwrap().push(registration);
        tokens
    }

    fn exchange_tokens(
        &self,
        scopes: &[&str],
        audience: Option<&str>,
        capacity: NonZeroU32,
    ) -> ExchangeTokens {
        self.exchange_tokens_with(self.options(), scopes, audience, capacity)
    }

    fn exchange_tokens_with(
        &self,
        options: Options,
        scopes: &[&str],
        audience: Option<&str>,
        capacity: NonZeroU32,
    ) -> ExchangeTokens {
        let registration = prepare_registration(options);
        let tokens = registration
            .exchange_tokens(
                exchange_target(scopes, audience),
                self.resource_origin(),
                capacity,
            )
            .unwrap();
        self.registrations.lock().unwrap().push(registration);
        tokens
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
        Client::try_new_for_test_http(self.origin.as_str(), TOKEN_LIMITS).unwrap()
    }

    fn service_client(&self) -> ServiceHttpClient {
        self.service_tokens(&[])
            .http(self.resource_client())
            .unwrap()
    }

    fn exchange_client(&self) -> ExchangeHttpClient {
        self.exchange_tokens(&[], None, EXCHANGE_CAPACITY)
            .http(self.resource_client())
            .unwrap()
    }

    async fn send(&self, client: &ServiceHttpClient) -> Result<Response<Bytes>, Error> {
        self.send_request(client, self.request()).await
    }

    async fn send_request(
        &self,
        client: &ServiceHttpClient,
        request: Request<Bytes>,
    ) -> Result<Response<Bytes>, Error> {
        client
            .execute(&timeout_context(Duration::from_secs(10)), request)
            .await
    }

    async fn send_exchange(
        &self,
        client: &ExchangeHttpClient,
        subject_token: &str,
    ) -> Result<Response<Bytes>, Error> {
        self.send_exchange_request(client, self.request(), subject_token)
            .await
    }

    async fn send_exchange_request(
        &self,
        client: &ExchangeHttpClient,
        request: Request<Bytes>,
        subject_token: &str,
    ) -> Result<Response<Bytes>, Error> {
        client
            .execute(
                &timeout_context(Duration::from_secs(10)),
                request,
                &subject(subject_token),
            )
            .await
    }

    fn request(&self) -> Request<Bytes> {
        Request::get(self.origin.join(RESOURCE_PATH).unwrap().as_str())
            .body(Bytes::new())
            .unwrap()
    }

    fn token_json(&self, status: StatusCode, body: &serde_json::Value) {
        *self.state.token_response.lock().unwrap() = json_response(status, body);
    }

    fn token_raw(&self, response: Vec<u8>) {
        *self.state.token_response.lock().unwrap() = response;
    }

    fn resource_status(&self, status: StatusCode) {
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
        take_event(&self.state.token_received).await;
    }

    async fn token_completed(&self) {
        take_event(&self.state.token_completed).await;
    }

    fn block_resources(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.state.resource_gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    async fn resource_received(&self) {
        take_event(&self.state.resource_received).await;
    }

    fn token_requests(&self) -> Vec<CapturedRequest> {
        self.state.token_requests.lock().unwrap().clone()
    }

    fn token_request_count(&self) -> usize {
        self.state.token_requests.lock().unwrap().len()
    }

    fn resource_requests(&self) -> Vec<CapturedRequest> {
        self.state.resource_requests.lock().unwrap().clone()
    }

    fn resource_request_count(&self) -> usize {
        self.state.resource_requests.lock().unwrap().len()
    }

    async fn drive_until_token_received<F>(&self, operation: Pin<&mut F>)
    where
        F: Future,
        F::Output: std::fmt::Debug,
    {
        tokio::select! {
            () = self.token_received() => {}
            result = operation => panic!("response must be gated: {result:?}"),
        }
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

fn service_target(scopes: &[&str]) -> ServiceTokenTarget {
    ServiceTokenTarget::try_new(scopes).unwrap()
}

fn exchange_target(scopes: &[&str], audience: Option<&str>) -> ExchangeTokenTarget {
    ExchangeTokenTarget::try_new(scopes, audience.map(str::to_owned)).unwrap()
}

async fn take_event(events: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(2), events.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
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
    let mut request_line = lines.next().unwrap().split_whitespace();
    let method = request_line.next().unwrap().to_owned();
    let target = request_line.next().unwrap().to_owned();
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
        method,
        target,
        headers,
        body: request[header_end..header_end + body_bytes].to_vec(),
    }
}

fn response(status: StatusCode, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        status.as_str(),
        status.canonical_reason().unwrap(),
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Vec<u8> {
    response(status, &serde_json::to_vec(body).unwrap())
}

fn exchange_response(access_token: &str) -> serde_json::Value {
    serde_json::json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 60,
        "issued_token_type": ACCESS_TOKEN_TYPE,
    })
}

fn prepare_registration(options: Options) -> Registration {
    Registration::prepare_for_test_http(options).unwrap()
}

fn timeout_context(after: Duration) -> OperationContext {
    OperationContext::with_timeout(after)
}

fn token_context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(10))
}

async fn advance(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

async fn poll_pending<F: Future + ?Sized>(mut future: Pin<&mut F>) {
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
