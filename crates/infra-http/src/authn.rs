//! Method-scoped inbound bearer authentication.
//!
//! Protected operations pass their `routes!` tuple to [`protect`]. The helper
//! validates that the OpenAPI operation makes the same protection decision as
//! the served route, then layers verification on registered methods only.
//! Unknown paths and method fallbacks therefore remain the router's 404/405.

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::{Failure, Principal, Verifier, parse_bearer};
use utoipa::openapi::path::{Operation, Paths};
use utoipa_axum::router::{UtoipaMethodRouter, UtoipaMethodRouterExt};

use crate::harden::RequestDeadline;
use crate::problem::{Code, Problem, SANITIZED_DETAIL};
use crate::request_id;

/// Verification outcomes at the HTTP authentication boundary.
pub const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

const PROTECTED_STATUSES: &[&str] = &["400", "401", "403", "431", "503", "504"];
const AUTHENTICATION_REQUIRED_DETAIL: &str = "bearer authentication is required";
const AUTHENTICATION_MALFORMED_DETAIL: &str = "bearer authentication is malformed";
const AUTHENTICATION_OVERSIZE_DETAIL: &str = "bearer authentication is too large";
const AUTHENTICATION_INVALID_DETAIL: &str = "bearer authentication is invalid";
const AUTHENTICATION_UNAVAILABLE_DETAIL: &str = "bearer authentication is unavailable";

/// A principal verified by the method-scoped authentication middleware.
///
/// The wrapped principal is private, so handlers can inspect only identity
/// values the selected verifier accepted. The extractor never parses an
/// inbound header and cannot manufacture an anonymous identity.
#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(Principal);

impl VerifiedPrincipal {
    /// The exact issuer that the verifier accepted.
    #[must_use]
    pub fn issuer(&self) -> &str {
        self.0.issuer()
    }

    /// The verified subject, when present in the selected token profile.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.0.subject()
    }

    /// The verified client identity, when present in the selected token profile.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.0.client_id()
    }
}

impl<S> FromRequestParts<S> for VerifiedPrincipal
where
    S: Send + Sync,
{
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Principal>()
                .cloned()
                .map(Self)
                .ok_or_else(|| {
                    Problem::new(Code::InternalServerError)
                        .detail(SANITIZED_DETAIL)
                        .request_id(request_id::request_id(&parts.extensions))
                        .into_response()
                }),
        )
    }
}

/// The route tuple did not prove the protected contract that its middleware
/// would enforce. This remains a sanitized composition error, never a route
/// that silently becomes public.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("protected route contract is invalid")]
pub struct ProtectError;

/// Validate and protect one `routes!` tuple.
///
/// The supported tuple has one documented path and one operation. Applying
/// the middleware with [`axum::routing::MethodRouter::route_layer`] scopes it
/// to registered methods while leaving the method fallback untouched.
///
/// # Errors
///
/// Returns [`ProtectError`] when the route's OpenAPI metadata does not declare
/// the protected bearer contract that this middleware enforces.
pub fn protect<S>(
    routes: UtoipaMethodRouter<S>,
    verifier: Verifier,
) -> Result<UtoipaMethodRouter<S>, ProtectError>
where
    S: Send + Sync + Clone + 'static,
{
    if !is_protected_route(&routes.1) {
        return Err(ProtectError);
    }
    metrics::describe_counter!(
        AUTHN_VERIFICATIONS_METRIC,
        metrics::Unit::Count,
        "Inbound bearer-authentication verification outcomes."
    );
    Ok(routes.map(|method_router| {
        method_router.route_layer(middleware::from_fn_with_state(verifier, authenticate))
    }))
}

async fn authenticate(
    State(verifier): State<Verifier>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request_id::request_id(request.extensions());
    let mut metric = VerificationMetric::new();
    request.extensions_mut().remove::<Principal>();
    request.extensions_mut().remove::<VerifiedPrincipal>();
    let Some(deadline) = request
        .extensions()
        .get::<RequestDeadline>()
        .map(RequestDeadline::at)
    else {
        metric.wiring_failure();
        return Problem::new(Code::InternalServerError)
            .detail(SANITIZED_DETAIL)
            .request_id(request_id)
            .into_response();
    };
    let token = match parse_bearer(
        request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(axum::http::HeaderValue::as_bytes),
    ) {
        Ok(token) => token,
        Err(failure) => {
            metric.failure(failure);
            return failure_response(failure, request_id);
        }
    };
    let verification = verifier.verify(&token, deadline).await;
    if tokio::time::Instant::now() >= deadline {
        // The tower timeout owns the final 504. Do not race it with a 503
        // after a provider/waiter reports its exhausted request budget.
        return std::future::pending().await;
    }
    let principal = match verification {
        Ok(principal) => principal,
        Err(failure) => {
            metric.failure(failure);
            return failure_response(failure, request_id);
        }
    };

    metric.success();
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn failure_response(failure: Failure, request_id: Option<String>) -> Response {
    let (code, detail, challenge) = match failure {
        Failure::Missing => (
            Code::AuthenticationRequired,
            AUTHENTICATION_REQUIRED_DETAIL,
            Some("Bearer"),
        ),
        Failure::Malformed => (
            Code::AuthenticationMalformed,
            AUTHENTICATION_MALFORMED_DETAIL,
            None,
        ),
        Failure::Oversize => (
            Code::AuthenticationOversize,
            AUTHENTICATION_OVERSIZE_DETAIL,
            None,
        ),
        Failure::Invalid => (
            Code::AuthenticationInvalid,
            AUTHENTICATION_INVALID_DETAIL,
            Some("Bearer error=\"invalid_token\""),
        ),
        Failure::Unavailable => (
            Code::AuthenticationUnavailable,
            AUTHENTICATION_UNAVAILABLE_DETAIL,
            None,
        ),
    };
    let mut response = Problem::new(code)
        .detail(detail)
        .request_id(request_id)
        .into_response();
    if let Some(challenge) = challenge {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static(challenge),
        );
    }
    response
}

fn is_protected_route(paths: &Paths) -> bool {
    if paths.paths.len() != 1 {
        return false;
    }
    let Some(path) = paths.paths.values().next() else {
        return false;
    };
    let operations = [
        path.get.as_ref(),
        path.put.as_ref(),
        path.post.as_ref(),
        path.delete.as_ref(),
        path.options.as_ref(),
        path.head.as_ref(),
        path.patch.as_ref(),
        path.trace.as_ref(),
    ];
    let mut operations = operations.into_iter().flatten();
    let Some(operation) = operations.next() else {
        return false;
    };
    operations.next().is_none() && is_protected_operation(operation)
}

fn is_protected_operation(operation: &Operation) -> bool {
    let decision = operation
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("x-security-decision"));
    let decision_is_protected =
        decision
            .and_then(serde_json::Value::as_object)
            .is_some_and(|decision| {
                decision.get("exposure").and_then(serde_json::Value::as_str) == Some("protected")
                    && decision
                        .get("rationale")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|rationale| !rationale.trim().is_empty())
            });
    if !decision_is_protected {
        return false;
    }
    let security_is_bearer_only = operation
        .security
        .as_ref()
        .and_then(|security| serde_json::to_value(security).ok())
        .and_then(|security| security.as_array().cloned())
        .is_some_and(|requirements| {
            !requirements.is_empty()
                && requirements.iter().all(|requirement| {
                    requirement.as_object().is_some_and(|requirement| {
                        requirement.len() == 1
                            && requirement.get("bearerAuth")
                                == Some(&serde_json::Value::Array(Vec::new()))
                    })
                })
        });
    if !security_is_bearer_only {
        return false;
    }
    let Ok(serde_json::Value::Object(responses)) = serde_json::to_value(&operation.responses)
    else {
        return false;
    };
    PROTECTED_STATUSES
        .iter()
        .all(|status| responses.contains_key(*status))
}

struct VerificationMetric {
    recorded: bool,
}

impl VerificationMetric {
    const fn new() -> Self {
        Self { recorded: false }
    }

    fn success(&mut self) {
        record_verification("success", "none");
        self.recorded = true;
    }

    fn failure(&mut self, failure: Failure) {
        record_verification("failure", failure_class(failure));
        self.recorded = true;
    }

    fn wiring_failure(&mut self) {
        record_verification("failure", "wiring");
        self.recorded = true;
    }
}

impl Drop for VerificationMetric {
    fn drop(&mut self) {
        if !self.recorded {
            record_verification("cancelled", "cancelled");
        }
    }
}

fn record_verification(result: &'static str, failure: &'static str) {
    metrics::counter!(
        AUTHN_VERIFICATIONS_METRIC,
        "transport" => "http",
        "result" => result,
        "failure" => failure
    )
    .increment(1);
}

const fn failure_class(failure: Failure) -> &'static str {
    match failure {
        Failure::Missing => "missing",
        Failure::Malformed => "malformed",
        Failure::Oversize => "oversize",
        Failure::Invalid => "invalid",
        Failure::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    // template:begin oidc-introspection:http-introspection-fixture-std-imports
    use std::sync::Arc;
    // template:end oidc-introspection:http-introspection-fixture-std-imports
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::header::{CONTENT_TYPE, WWW_AUTHENTICATE};
    use axum::http::{HeaderMap, Method, Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use http_body_util::BodyExt;
    // template:begin oidc-introspection:http-introspection-fixture-imports
    use secrecy::SecretString;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::rustls::crypto::aws_lc_rs;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_util::{sync::CancellationToken, task::TaskTracker};
    // template:end oidc-introspection:http-introspection-fixture-imports
    use tower::ServiceExt;
    use utoipa_axum::{router::OpenApiRouter, routes};

    use super::*;
    use crate::{HardenOptions, harden};

    // template:begin oidc-introspection:http-introspection-fixture-material
    const FIXTURE_HOST: &str = "authn.fixture.test";
    const FIXTURE_ROOT_DER: &[u8] =
        include_bytes!("../../infra-bearerauthn/tests/fixtures/authn-fixture-root.der");
    const FIXTURE_CERT_DER: &[u8] =
        include_bytes!("../../infra-bearerauthn/tests/fixtures/authn-fixture-cert.der");
    const FIXTURE_KEY_DER: &[u8] =
        include_bytes!("../../infra-bearerauthn/tests/fixtures/authn-fixture-key.der");
    // template:end oidc-introspection:http-introspection-fixture-material

    #[utoipa::path(
        get,
        path = "/_test/protected",
        operation_id = "infraHttpTestProtected",
        summary = "Test-only protected HTTP composition",
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only route exercising the production method-scoped middleware"
        }))),
        responses(
            (status = 200, description = "verified", content_type = "text/plain", body = String),
            crate::problem::responses::ProtectedOperationProblemResponses,
        )
    )]
    async fn protected(principal: VerifiedPrincipal, headers: HeaderMap) -> impl IntoResponse {
        if headers.contains_key(AUTHORIZATION) || principal.issuer().is_empty() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        (StatusCode::OK, "verified").into_response()
    }

    #[utoipa::path(
        get,
        path = "/_test/public",
        operation_id = "infraHttpTestPublic",
        security(),
        extensions(("x-security-decision" = json!({
            "exposure": "public",
            "rationale": "a public route must never receive protected middleware"
        }))),
        responses((status = 200, description = "public", content_type = "text/plain", body = String))
    )]
    async fn declared_public() -> &'static str {
        "public"
    }

    #[utoipa::path(
        get,
        path = "/_test/incomplete",
        operation_id = "infraHttpTestIncompleteProtected",
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "negative metadata test"
        }))),
        responses((status = 200, description = "incomplete", content_type = "text/plain", body = String))
    )]
    async fn incomplete_protected() -> &'static str {
        "incomplete"
    }

    fn options() -> HardenOptions {
        HardenOptions {
            max_body_bytes: 1024,
            request_timeout: Duration::from_millis(100),
            max_in_flight: NonZeroU32::new(2),
            log_health_probes: false,
        }
    }

    fn protected_router(verifier: Verifier) -> Router {
        protected_router_with_timeout(verifier, options().request_timeout)
    }

    fn protected_router_with_timeout(verifier: Verifier, request_timeout: Duration) -> Router {
        let routes = OpenApiRouter::new()
            .routes(protect(routes!(protected), verifier).expect("protected test metadata"))
            .route("/_test/public", get(|| async { "public" }));
        let (routes, _document) = routes.split_for_parts();
        let mut options = options();
        options.request_timeout = request_timeout;
        harden(routes, &options)
    }

    fn request(method: Method, path: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .expect("request")
    }

    async fn problem_code(response: axum::response::Response) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("complete problem body")
            .to_bytes();
        serde_json::from_slice::<serde_json::Value>(&bytes).expect("problem JSON")["code"]
            .as_str()
            .expect("problem code")
            .to_owned()
    }

    // template:begin oidc-introspection:http-introspection-fixture-server
    #[derive(Clone, Copy)]
    enum FixtureReply {
        Active,
        Inactive,
        Stall,
    }

    fn fixture_tls_config() -> ServerConfig {
        ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("fixture TLS protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(FIXTURE_CERT_DER)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(FIXTURE_KEY_DER)),
            )
            .expect("fixture certificate and key")
    }

    async fn start_fixture_server(
        reply: FixtureReply,
    ) -> (
        std::net::SocketAddr,
        JoinHandle<Vec<u8>>,
        oneshot::Receiver<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let acceptor = TlsAcceptor::from(Arc::new(fixture_tls_config()));
        let (accepted_send, accepted) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("fixture accepts request");
            let _ = accepted_send.send(());
            let mut socket = acceptor
                .accept(socket)
                .await
                .expect("fixture TLS handshake");
            let mut request = vec![0_u8; 4096];
            let bytes = socket
                .read(&mut request)
                .await
                .expect("fixture reads request");
            request.truncate(bytes);

            let body = match reply {
                FixtureReply::Active => {
                    r#"{"active":true,"iss":"https://issuer.example","aud":"service","exp":2147483647,"sub":"fixture-subject"}"#
                }
                FixtureReply::Inactive => r#"{"active":false}"#,
                FixtureReply::Stall => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    r#"{"active":false}"#
                }
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
            request
        });
        (address, task, accepted)
    }

    async fn fixture_verifier(
        reply: FixtureReply,
    ) -> (
        Verifier,
        CancellationToken,
        TaskTracker,
        JoinHandle<Vec<u8>>,
        oneshot::Receiver<()>,
    ) {
        let (address, server, accepted) = start_fixture_server(reply).await;
        let tracker = TaskTracker::new();
        let cancel = CancellationToken::new();
        let transport = infra_bearerauthn::test_support::FixtureTransport::new(
            tracker.clone(),
            cancel.child_token(),
            FIXTURE_HOST,
            address,
            FIXTURE_ROOT_DER,
        )
        .expect("fixture transport");
        let verifier = infra_bearerauthn::test_support::prepare_introspection_with_fixture(
            infra_bearerauthn::IntrospectionOptions {
                issuer: "https://issuer.example".to_owned(),
                audience: "service".to_owned(),
                endpoint: format!("https://{FIXTURE_HOST}/introspect"),
                client_id: "fixture-client".to_owned(),
                client_secret: SecretString::from("fixture-secret"),
            },
            transport,
        )
        .expect("fixture verifier");
        (verifier, cancel, tracker, server, accepted)
    }

    async fn finish_fixture(
        cancel: CancellationToken,
        tracker: TaskTracker,
        server: JoinHandle<Vec<u8>>,
    ) -> Vec<u8> {
        let request = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("fixture server finishes")
            .expect("fixture server task");
        cancel.cancel();
        tracker.close();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .expect("fixture DNS work joins");
        request
    }

    async fn abort_fixture(
        cancel: CancellationToken,
        tracker: TaskTracker,
        server: JoinHandle<Vec<u8>>,
    ) {
        server.abort();
        let _ = server.await;
        cancel.cancel();
        tracker.close();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .expect("fixture DNS work joins");
    }
    // template:end oidc-introspection:http-introspection-fixture-server

    #[tokio::test]
    async fn protected_methods_fail_closed_without_changing_public_or_method_fallbacks() {
        let missing = protected_router(Verifier::disabled())
            .oneshot(request(Method::GET, "/_test/protected"))
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            missing.headers().get(CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static(
                "application/problem+json"
            ))
        );
        assert_eq!(
            missing.headers().get(WWW_AUTHENTICATE),
            Some(&axum::http::HeaderValue::from_static("Bearer"))
        );
        assert_eq!(problem_code(missing).await, "authentication_required");

        let malformed = protected_router(Verifier::disabled())
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/protected")
                    .header(AUTHORIZATION, "Basic not-bearer")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert!(malformed.headers().get(WWW_AUTHENTICATE).is_none());
        assert_eq!(problem_code(malformed).await, "authentication_malformed");

        let disabled = protected_router(Verifier::disabled())
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/protected")
                    .header(AUTHORIZATION, "Bearer opaque-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(disabled.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(disabled.headers().get(WWW_AUTHENTICATE).is_none());
        assert_eq!(problem_code(disabled).await, "authentication_unavailable");

        let oversize = protected_router(Verifier::disabled())
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/protected")
                    .header(
                        AUTHORIZATION,
                        format!("Bearer {}", "a".repeat(32 * 1024 + 1)),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            oversize.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
        assert!(oversize.headers().get(WWW_AUTHENTICATE).is_none());
        assert_eq!(problem_code(oversize).await, "authentication_oversize");

        let wrong_method = protected_router(Verifier::disabled())
            .oneshot(request(Method::POST, "/_test/protected"))
            .await
            .expect("response");
        assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(problem_code(wrong_method).await, "method_not_allowed");

        let missing = protected_router(Verifier::disabled())
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/missing")
                    .header(AUTHORIZATION, "Bearer opaque-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(problem_code(missing).await, "not_found");

        let public = protected_router(Verifier::disabled())
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/public")
                    .header(AUTHORIZATION, "Basic not-bearer")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(public.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_route_without_hardened_deadline_is_a_sanitized_wiring_failure() {
        let routes = OpenApiRouter::new()
            .routes(
                protect(routes!(protected), Verifier::disabled()).expect("protected test metadata"),
            )
            .split_for_parts()
            .0;
        let response = routes
            .oneshot(request(Method::GET, "/_test/protected"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(problem_code(response).await, "internal_error");
    }

    #[test]
    fn protected_route_metadata_rejects_public_and_incomplete_tuples() {
        assert!(protect::<()>(routes!(declared_public), Verifier::disabled()).is_err());
        assert!(protect::<()>(routes!(incomplete_protected), Verifier::disabled()).is_err());
    }

    // template:begin oidc-introspection:http-introspection-fixture-route-tests
    #[tokio::test]
    async fn mounted_real_introspection_verifies_identity_removes_authorization_and_redacts_denials()
     {
        let (verifier, cancel, tracker, server, _accepted) =
            fixture_verifier(FixtureReply::Active).await;
        // This proves identity and wire semantics, not a one-second TLS SLO.
        // Keep the parent above the provider's real three-second bound so
        // platform certificate validation is not confused with this oracle.
        let success = protected_router_with_timeout(verifier, Duration::from_secs(5))
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/protected")
                    .header(AUTHORIZATION, "Bearer opaque-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(success.status(), StatusCode::OK);
        let request = finish_fixture(cancel, tracker, server).await;
        let request = String::from_utf8(request).expect("fixture request is HTTP text");
        assert!(
            request.starts_with("POST /introspect HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("authorization: Basic "), "{request}");
        assert!(!request.contains("Bearer opaque-token"), "{request}");

        let (verifier, cancel, tracker, server, _accepted) =
            fixture_verifier(FixtureReply::Inactive).await;
        let denied = protected_router_with_timeout(verifier, Duration::from_secs(5))
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/protected")
                    .header(AUTHORIZATION, "Bearer opaque-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.headers().get(WWW_AUTHENTICATE),
            Some(&axum::http::HeaderValue::from_static(
                "Bearer error=\"invalid_token\""
            ))
        );
        let denied_body = denied
            .into_body()
            .collect()
            .await
            .expect("complete denial body")
            .to_bytes();
        let denied_body = String::from_utf8(denied_body.to_vec()).expect("problem is UTF-8 JSON");
        assert!(denied_body.contains("authentication_invalid"));
        assert!(!denied_body.contains("opaque-token"));
        assert!(!denied_body.contains(FIXTURE_HOST));
        let _ = finish_fixture(cancel, tracker, server).await;
    }

    #[tokio::test]
    async fn mounted_real_introspection_defers_to_the_enclosing_request_deadline() {
        let (verifier, cancel, tracker, server, accepted) =
            fixture_verifier(FixtureReply::Stall).await;
        let response = protected_router_with_timeout(verifier, Duration::from_millis(40))
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/_test/protected")
                    .header(AUTHORIZATION, "Bearer opaque-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(problem_code(response).await, "request_timeout");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), accepted)
                .await
                .is_err(),
            "exhausted reserve must not contact the fixture"
        );
        abort_fixture(cancel, tracker, server).await;
    }
    // template:end oidc-introspection:http-introspection-fixture-route-tests
}
