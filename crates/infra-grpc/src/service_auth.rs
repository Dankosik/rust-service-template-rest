use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use foldhash::HashMap;
use futures_util::future::BoxFuture;
use http::header::AUTHORIZATION;
use infra_serviceauth::{Caller, ReceiverTokens, ServiceName, Verifier};
use tonic::body::Body;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::{Client, Failure};

pub const REFLECTION_PATHS: [&str; 2] = [
    "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
];

const NOT_ADMITTED_DETAIL: &str = "the verified caller is not admitted to this method";

pub(crate) struct CallerAdmission {
    verifier: Option<Verifier>,
    caller_rules: HashMap<String, Box<[ServiceName]>>,
}

impl CallerAdmission {
    pub(crate) fn new(
        verifier: Option<Verifier>,
        caller_rules: HashMap<String, Box<[ServiceName]>>,
    ) -> Self {
        Self {
            verifier,
            caller_rules,
        }
    }

    pub(crate) const fn verifies(&self) -> bool {
        self.verifier.is_some()
    }
}

pub(crate) async fn admit_caller(
    State(admission): State<Arc<CallerAdmission>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(verifier) = &admission.verifier else {
        request.headers_mut().remove(AUTHORIZATION);
        return next.run(request).await;
    };
    let Some(context) = crate::router::opening_context(&request).cloned() else {
        return crate::status::internal_status().into_http();
    };
    if let Some(reason) = context.stopped() {
        return crate::call::Side::Server.stopped(reason).into_http();
    }
    let authorization = request
        .headers()
        .get_all(AUTHORIZATION)
        .iter()
        .map(http::HeaderValue::as_bytes);
    let result = verifier.authenticate(authorization);
    if let Some(reason) = context.stopped() {
        return crate::call::Side::Server.stopped(reason).into_http();
    }
    let caller = match result {
        Ok(caller) => caller,
        Err(failure) => return crate::router::reject(request, verification_status(failure)).await,
    };
    let path = request.uri().path();
    let rule = admission.caller_rules.get(path);
    if !rule.is_some_and(|callers| callers.contains(caller.name())) {
        let method = if rule.is_some() { path } else { "unknown" };
        tracing::warn!(
            caller = %caller.name(),
            method,
            "service_auth_caller_not_admitted"
        );
        let status = Failure::new(service_failure::Code::Forbidden)
            .into_status_as(tonic::Code::PermissionDenied, NOT_ADMITTED_DETAIL);
        return crate::router::reject(request, status).await;
    }
    if let Some(reason) = context.stopped() {
        return crate::call::Side::Server.stopped(reason).into_http();
    }
    tracing::Span::current().set_attribute("peer.service", Arc::<str>::from(caller.name().clone()));
    request.extensions_mut().insert::<Caller>(caller);
    request.headers_mut().remove(AUTHORIZATION);
    next.run(request).await
}

fn verification_status(failure: infra_serviceauth::Failure) -> tonic::Status {
    Failure::new(match failure {
        infra_serviceauth::Failure::Missing
        | infra_serviceauth::Failure::Malformed
        | infra_serviceauth::Failure::Invalid => service_failure::Code::Unauthorized,
        infra_serviceauth::Failure::Unavailable => service_failure::Code::ServiceUnavailable,
    })
    .into()
}

#[derive(Clone, Debug)]
pub struct ServiceTokenClient {
    tokens: ReceiverTokens,
    resource: Client,
}

impl Client {
    #[must_use]
    pub fn with_service_tokens(self, tokens: ReceiverTokens) -> ServiceTokenClient {
        ServiceTokenClient {
            tokens,
            resource: self,
        }
    }
}

impl tower::Service<http::Request<Body>> for ServiceTokenClient {
    type Response = http::Response<Body>;
    type Error = tonic::Status;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let mut prepared = self.resource.prepare_call(request);
        if prepared.headers_mut().contains_key(AUTHORIZATION) {
            return Box::pin(std::future::ready(Err(prepared.refuse(
                tonic::Status::internal("authorization conflicts with the service token"),
            ))));
        }
        let tokens = self.tokens.clone();
        Box::pin(prepared.send_inner(
            move |headers| {
                let authorization = tokens.authorization().map_err(|error| {
                    let mut status = tonic::Status::unavailable("service token unavailable");
                    status.set_source(Arc::new(error));
                    status
                })?;
                if let Some(value) = authorization {
                    headers.insert(AUTHORIZATION, value);
                }
                Ok(())
            },
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::StreamExt as _;
    use infra_serviceauth::{
        JwtType, TrustRegistry,
        test_support::{TestKey, now_secs, registry},
    };
    use operation_context::OperationContext;
    use tower::ServiceExt as _;

    use super::*;

    const METHOD: &str = "/example.v1.EchoService/Unary";
    const TYPE: &str = "example-svc+jwt";

    fn orders_only_router() -> (axum::Router, String, Arc<AtomicBool>) {
        let orders = TestKey::new("orders/2026-10");
        let billing = TestKey::new("billing/2026-10");
        let trust = TrustRegistry::parse(&registry(&[
            ("orders", &[&orders]),
            ("billing", &[&billing]),
        ]))
        .unwrap();
        let verifier = Verifier::new(
            ServiceName::parse("catalog").unwrap(),
            JwtType::parse(TYPE).unwrap(),
            trust,
        );
        let mut rules = HashMap::default();
        rules.insert(
            METHOD.to_owned(),
            Box::from([ServiceName::parse("orders").unwrap()]),
        );
        let admission = Arc::new(CallerAdmission::new(Some(verifier), rules));

        let reached_handler = Arc::new(AtomicBool::new(false));
        let handler_flag = Arc::clone(&reached_handler);
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new()
            .route(
                METHOD,
                axum::routing::post(move || {
                    let handler_flag = Arc::clone(&handler_flag);
                    async move {
                        handler_flag.store(true, Ordering::SeqCst);
                        tonic::Status::ok("").into_http::<Body>()
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                admission,
                admit_caller,
            ));

        let now = now_secs();
        let billing_token = billing.sign(
            &serde_json::json!({"alg": "ES256", "typ": TYPE, "kid": billing.kid()}),
            &serde_json::json!({
                "iss": "billing",
                "sub": "billing",
                "aud": "catalog",
                "iat": now,
                "exp": now + 60,
            }),
        );
        (app, format!("Bearer {billing_token}"), reached_handler)
    }

    #[tokio::test]
    async fn a_stopped_context_answers_before_caller_admission() {
        let (app, bearer, reached_handler) = orders_only_router();

        let body_polls = Arc::new(AtomicUsize::new(0));
        let make_request = |context: OperationContext, authorization: Option<&str>| {
            let polls = Arc::clone(&body_polls);
            let stream = futures_util::stream::iter(0..2).map(move |_| {
                polls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(Bytes::from_static(b"frame"))
            });
            let mut builder = http::Request::post(METHOD);
            if let Some(value) = authorization {
                builder = builder.header(AUTHORIZATION, value);
            }
            let mut request = builder.body(axum::body::Body::from_stream(stream)).unwrap();
            request.extensions_mut().insert(context);
            request
        };

        for authorization in [Some(bearer.as_str()), None] {
            let response = app
                .clone()
                .oneshot(make_request(
                    OperationContext::with_timeout(Duration::ZERO),
                    authorization,
                ))
                .await
                .unwrap();
            assert_eq!(
                tonic::Status::from_header_map(response.headers())
                    .unwrap()
                    .code(),
                tonic::Code::DeadlineExceeded
            );
        }

        let cancelled_context = OperationContext::unbounded();
        cancelled_context.cancellation().cancel();
        let cancelled = app
            .clone()
            .oneshot(make_request(cancelled_context, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            tonic::Status::from_header_map(cancelled.headers())
                .unwrap()
                .code(),
            tonic::Code::Cancelled
        );

        assert!(!reached_handler.load(Ordering::SeqCst));
        assert_eq!(body_polls.load(Ordering::SeqCst), 0);

        let live = app
            .oneshot(make_request(
                OperationContext::with_timeout(Duration::from_secs(5)),
                Some(&bearer),
            ))
            .await
            .unwrap();
        assert_eq!(
            tonic::Status::from_header_map(live.headers())
                .unwrap()
                .code(),
            tonic::Code::PermissionDenied
        );
    }
}
