use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::task::AtomicWaker;
use http::header::HeaderName;
use http::{Request, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tonic::body::Body;
use tonic::transport::{Channel, Endpoint};
use tower::{Service, ServiceExt as _};

use crate::{Error, ServiceDescriptor};

const MAX_METADATA_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Absolute custody supplied by the business caller.
///
/// The transport only clamps an operation to an already-owned parent deadline;
/// it never installs a default application deadline, retries, or refreshes it
/// while a streaming RPC remains open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Operation {
    pub deadline: Instant,
}

impl Operation {
    #[must_use]
    pub fn clamped(self, parent: Instant) -> Self {
        Self {
            deadline: self.deadline.min(parent),
        }
    }

    #[must_use]
    pub fn is_expired(self) -> bool {
        Instant::now() >= self.deadline
    }
}

/// Explicit security selected for one trusted operator destination.
#[derive(Clone, Debug)]
pub enum ClientSecurity {
    Plaintext,
    Tls(ClientTlsMaterial),
}

/// Trust and optional client identity material for normal TLS verification.
/// The service config owner admits these values; this adapter never loads a
/// path or disables hostname verification.
#[derive(Clone)]
pub struct ClientTlsMaterial {
    pub ca_certificate_pem: Option<Vec<u8>>,
    pub certificate_pem: Option<Vec<u8>>,
    pub private_key_pem: Option<Vec<u8>>,
}

impl fmt::Debug for ClientTlsMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientTlsMaterial")
            .field("has_ca_certificate", &self.ca_certificate_pem.is_some())
            .field("has_certificate", &self.certificate_pem.is_some())
            .field("has_private_key", &self.private_key_pem.is_some())
            .finish()
    }
}

/// One lazy, shared channel for a configured dependency.
#[derive(Clone)]
pub struct Client {
    channel: Channel,
    catalog: Arc<BTreeMap<&'static str, crate::Method>>,
    tracker: TaskTracker,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Client { channel: lazy }")
    }
}

impl Client {
    /// Parses an explicit trusted destination and constructs a lazy channel.
    /// It performs neither DNS nor network I/O.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] for an invalid destination or
    /// unusable TLS trust or client identity material.
    pub fn new(destination: &str, security: ClientSecurity) -> Result<Self, Error> {
        let endpoint = Endpoint::from_shared(destination.to_owned())
            .map_err(|_| Error::InvalidConfiguration)?;
        let channel = match security {
            ClientSecurity::Plaintext => endpoint.connect_lazy(),
            ClientSecurity::Tls(material) => crate::tls::client_channel(&endpoint, material)?,
        };
        Ok(Self {
            channel,
            catalog: Arc::new(BTreeMap::new()),
            tracker: TaskTracker::new(),
        })
    }

    #[must_use]
    pub const fn max_message_bytes() -> usize {
        MAX_MESSAGE_BYTES
    }

    /// Admits one generated, descriptor-derived method catalog.  Construction
    /// stays lazy; registration performs no resolver or connection work.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRegistration`] if the catalog does not match its
    /// descriptor set, contains invalid paths, or repeats a registered method.
    pub fn with_service(mut self, descriptor: ServiceDescriptor) -> Result<Self, Error> {
        if !crate::registration::descriptor_matches_fds(descriptor) {
            return Err(Error::InvalidRegistration);
        }
        let catalog = Arc::make_mut(&mut self.catalog);
        for method in descriptor.methods() {
            let prefix = format!("/{}/", descriptor.name());
            if !method.path().starts_with(&prefix)
                || method.path().len() == prefix.len()
                || catalog.insert(method.path(), *method).is_some()
            {
                return Err(Error::InvalidRegistration);
            }
        }
        Ok(self)
    }
}

impl Service<Request<Body>> for Client {
    type Response = Response<Body>;
    type Error = tonic::Status;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, tonic::Status>> + Send>>;

    /// Readiness intentionally does not connect.  The owned operation deadline
    /// covers the cloned channel's actual readiness/queue/connect wait in
    /// [`Self::call`].
    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let parent_deadline = crate::call::CallState::current().and_then(|state| state.deadline());
        let operation = request
            .extensions_mut()
            .remove::<Operation>()
            .map(|operation| parent_deadline.map_or(operation, |parent| operation.clamped(parent)));
        let mut channel = self.channel.clone();
        let catalog = Arc::clone(&self.catalog);
        let tracker = self.tracker.clone();
        Box::pin(async move {
            let operation = operation.ok_or_else(|| tonic::Status::internal("request failed"))?;
            let now = Instant::now();
            if now >= operation.deadline {
                return Err(tonic::Status::deadline_exceeded(
                    "request deadline exceeded",
                ));
            }
            if metadata_bytes(request.headers()) > MAX_METADATA_BYTES {
                return Err(tonic::Status::resource_exhausted(
                    "request metadata is too large",
                ));
            }
            let Some(method) = catalog.get(request.uri().path()).copied() else {
                return Err(tonic::Status::internal("request failed"));
            };
            let observation = crate::observe::Observation::client(method);

            let remaining = operation.deadline.saturating_duration_since(now);
            set_grpc_timeout(&mut request, remaining)?;
            observation.inject(request.headers_mut());
            let exchange = async move {
                let ready = channel.ready().await.map_err(transport_status)?;
                ready.call(request).await.map_err(transport_status)
            };
            let response = tokio::time::timeout_at(operation.deadline, exchange)
                .await
                .unwrap_or_else(|_| {
                    Err(tonic::Status::deadline_exceeded(
                        "request deadline exceeded",
                    ))
                });
            match response {
                Ok(response) => {
                    let (parts, body) = response.into_parts();
                    let body = Body::new(ClientBody::new(
                        body,
                        operation.deadline,
                        observation,
                        &tracker,
                        crate::observe::status_code(&parts.headers),
                    ));
                    Ok(Response::from_parts(parts, body))
                }
                Err(status) => {
                    observation.finish(status.code());
                    Err(status)
                }
            }
        })
    }
}

struct ClientBodyState {
    body: Mutex<Option<Body>>,
    terminal: Mutex<Option<tonic::Status>>,
    cancel: CancellationToken,
    observation: Mutex<Option<crate::observe::Observation>>,
    waker: AtomicWaker,
}

struct ClientBody {
    state: Arc<ClientBodyState>,
    emitted_terminal: bool,
}

impl ClientBody {
    fn new(
        body: Body,
        deadline: Instant,
        observation: crate::observe::Observation,
        tracker: &TaskTracker,
        initial_status: Option<tonic::Code>,
    ) -> Self {
        let state = Arc::new(ClientBodyState {
            body: Mutex::new(Some(body)),
            terminal: Mutex::new(None),
            cancel: CancellationToken::new(),
            observation: Mutex::new(Some(observation)),
            waker: AtomicWaker::new(),
        });
        if let Some(code) = initial_status {
            state.finish(Some(tonic::Status::new(code, "")));
        }
        let deadline_state = Arc::clone(&state);
        let cancel = state.cancel.child_token();
        tracker.spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {},
                () = tokio::time::sleep_until(deadline) => {
                    deadline_state.finish(Some(tonic::Status::deadline_exceeded("request deadline exceeded")));
                }
            }
        });
        Self {
            state,
            emitted_terminal: initial_status.is_some(),
        }
    }

    fn terminal_frame(
        &mut self,
        status: tonic::Status,
    ) -> Poll<Option<Result<Frame<Bytes>, tonic::Status>>> {
        if self.emitted_terminal {
            return Poll::Ready(None);
        }
        self.emitted_terminal = true;
        let (parts, ()) = status.into_http::<()>().into_parts();
        Poll::Ready(Some(Ok(Frame::trailers(parts.headers))))
    }
}

impl ClientBodyState {
    fn finish(&self, status: Option<tonic::Status>) -> tonic::Status {
        let mut terminal = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(status) = terminal.as_ref() {
            return status.clone();
        }
        let status = status.unwrap_or_else(|| tonic::Status::unknown("missing response status"));
        let code = status.code();
        *terminal = Some(status.clone());
        drop(terminal);
        self.cancel.cancel();
        let body = self
            .body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(body);
        if let Some(observation) = self
            .observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            observation.finish(code);
        }
        self.waker.wake();
        status
    }
}

impl HttpBody for ClientBody {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.emitted_terminal {
            return Poll::Ready(None);
        }
        self.state.waker.register(context.waker());
        let terminal = self
            .state
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(status) = terminal {
            return self.terminal_frame(status);
        }
        let polled = self
            .state
            .body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .map(|body| Pin::new(body).poll_frame(context));
        let terminal = self
            .state
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(status) = terminal {
            return self.terminal_frame(status);
        }
        match polled {
            Some(Poll::Ready(Some(Ok(frame)))) => {
                if let Some(trailers) = frame.trailers_ref() {
                    let code =
                        crate::observe::status_code(trailers).unwrap_or(tonic::Code::Unknown);
                    let status = self.state.finish(Some(tonic::Status::new(code, "")));
                    if status.code() != code {
                        return self.terminal_frame(status);
                    }
                    self.emitted_terminal = true;
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Poll::Ready(Some(Err(status)))) => {
                let status = self.state.finish(Some(tonic::Status::new(
                    status.code(),
                    service_failure::SANITIZED_DETAIL,
                )));
                self.terminal_frame(status)
            }
            Some(Poll::Ready(None)) | None => {
                let status = self.state.finish(None);
                self.terminal_frame(status)
            }
            Some(Poll::Pending) => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.emitted_terminal
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

impl Drop for ClientBody {
    fn drop(&mut self) {
        self.state
            .finish(Some(tonic::Status::cancelled("request cancelled")));
    }
}

fn transport_status(error: tonic::transport::Error) -> tonic::Status {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(error) = source {
        if let Some(status) = error.downcast_ref::<tonic::Status>() {
            return tonic::Status::new(status.code(), service_failure::SANITIZED_DETAIL);
        }
        source = error.source();
    }
    let code = tonic::Status::try_from_error(Box::new(error))
        .map_or(tonic::Code::Unavailable, |status| status.code());
    tonic::Status::new(code, "transport unavailable")
}

fn metadata_bytes(headers: &http::HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
    })
}

fn set_grpc_timeout(request: &mut Request<Body>, remaining: Duration) -> Result<(), tonic::Status> {
    if remaining.is_zero() {
        return Err(tonic::Status::deadline_exceeded(
            "request deadline exceeded",
        ));
    }
    let timeout = grpc_timeout(remaining);
    request.headers_mut().insert(
        HeaderName::from_static("grpc-timeout"),
        http::HeaderValue::from_bytes(timeout.as_bytes())
            .map_err(|_| tonic::Status::internal("request failed"))?,
    );
    debug_assert!(
        request
            .headers()
            .contains_key(HeaderName::from_static("grpc-timeout"))
    );
    Ok(())
}

fn grpc_timeout(duration: Duration) -> String {
    const MAX: u128 = 99_999_999;
    for (unit, value) in [
        ('n', duration.as_nanos()),
        ('u', duration.as_micros()),
        ('m', duration.as_millis()),
        ('S', u128::from(duration.as_secs())),
        ('M', u128::from(duration.as_secs() / 60)),
        ('H', u128::from(duration.as_secs() / (60 * 60))),
    ] {
        if value <= MAX {
            return format!("{value}{unit}");
        }
    }
    "99999999H".to_owned()
}
