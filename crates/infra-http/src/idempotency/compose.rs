//! Declaration checks and composition for idempotent route tuples, and the
//! key handling each composed route runs after authentication.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{FromRequestParts, Request, State};
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::Verifier;
use infra_idempotency_store::Store;
use utoipa::OpenApi as _;
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter, UtoipaMethodRouterExt};

use super::declaration::{self, AgreementError, Rule};
use super::execute::{Attempt, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, sanitized};
use super::identity::{self, Caller};
use super::openapi::{IdempotencyComponents, KEY_HEADER};
use crate::authn::{self, VerifiedPrincipal};
use crate::harden::RequestDeadline;
use crate::problem::{Code, Problem};
use crate::request_id;

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const INVALID_KEY_REASON: &str =
    "must be one Idempotency-Key field of 1 to 255 RFC 9110 token characters";

/// Declaration checks and composition for idempotent route tuples.
///
/// The composition root passes every idempotent operation's `routes!` tuple
/// through [`Composer::route`], merges [`Composer::components`] into the
/// contract, and checks the assembled document with [`Composer::agree`]
/// before readiness admission. An operation is served through the boundary
/// if and only if its generated operation declares `x-idempotent: true`.
#[derive(Debug)]
pub struct Composer {
    store: Store,
    verifier: Verifier,
    composed: BTreeSet<String>,
    failures: Vec<AgreementError>,
}

/// Whether the boundary serves any idempotent operation.
#[derive(Debug)]
pub enum Activation {
    /// No operation is composed: the boundary makes no database query,
    /// starts no task, and requires no configuration value.
    Inactive,
    /// At least one operation is composed; the store must pass its startup
    /// check before readiness admission.
    #[non_exhaustive]
    Active {
        store: Store,
        operations: NonZeroUsize,
    },
}

impl Composer {
    /// A composer whose routes arbitrate through `store` and authenticate
    /// with `verifier`.
    #[must_use]
    pub fn new(store: Store, verifier: Verifier) -> Self {
        Self {
            store,
            verifier,
            composed: BTreeSet::new(),
            failures: Vec::new(),
        }
    }

    /// A composer over an inert store and the disabled verifier, for
    /// rendering the document and for tests. It performs no I/O.
    #[must_use]
    pub fn inert() -> Self {
        Self::new(Store::inert(), Verifier::disabled())
    }

    /// Compose one idempotent `routes!` tuple: one path, one method.
    ///
    /// Key handling is layered first and bearer authentication
    /// ([`crate::protect`]) around it, so authentication runs before key
    /// handling and both run before the handler's extractors; method
    /// fallbacks stay 404 and 405. A tuple that breaks a declaration rule or
    /// the protected-operation contract is still returned, answering a
    /// sanitized 500 to every request, and [`Composer::agree`] refuses the
    /// document.
    pub fn route<S>(&mut self, routes: UtoipaMethodRouter<S>) -> UtoipaMethodRouter<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        metrics::describe_counter!(
            HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Outcomes of requests to idempotent operations, by outcome."
        );
        let refused = routes.clone();
        let operation = match declaration::check_route(&routes.1) {
            Ok(operation) => operation,
            Err(failure) => {
                self.failures.push(failure);
                return refuse(refused);
            }
        };
        let keys = KeyLayer {
            store: self.store.clone(),
            operation: Arc::from(operation.as_str()),
        };
        let layered = routes.map(|method_router| {
            method_router.route_layer(middleware::from_fn_with_state(keys, handle_key))
        });
        if let Ok(protected) = authn::protect(layered, self.verifier.clone()) {
            self.composed.insert(operation);
            protected
        } else {
            self.failures
                .push(AgreementError::new(operation, Rule::Protected));
            refuse(refused)
        }
    }

    /// The idempotency response components, as a router to merge into the
    /// contract: the family's only registration path.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "the family registers through the composer the contract receives, its only path"
    )]
    pub fn components<S>(&self) -> OpenApiRouter<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        OpenApiRouter::with_openapi(IdempotencyComponents::openapi())
    }

    /// Check the assembled document against the composed routes.
    ///
    /// # Errors
    ///
    /// Returns [`AgreementError`] when a composed tuple broke a rule, when
    /// an operation declaring `x-idempotent` is not composed or breaks a
    /// rule, when a composed operation is not declared, when another
    /// operation declares an `Idempotency-Key` header, or when the
    /// idempotency response components are missing.
    pub fn agree(self, document: &utoipa::openapi::OpenApi) -> Result<Activation, AgreementError> {
        if let Some(failure) = self.failures.into_iter().next() {
            return Err(failure);
        }
        declaration::agree(document, &self.composed)?;
        Ok(match NonZeroUsize::new(self.composed.len()) {
            Some(operations) => Activation::Active {
                store: self.store,
                operations,
            },
            None => Activation::Inactive,
        })
    }
}

/// The fail-closed form of a tuple that broke a rule: merged, so it is never
/// served silently, but every request answers a sanitized 500.
fn refuse<S>(routes: UtoipaMethodRouter<S>) -> UtoipaMethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    routes.map(|method_router| {
        method_router.route_layer(middleware::from_fn(|request: Request, _: Next| {
            std::future::ready(sanitized(request_id::request_id(request.extensions())))
        }))
    })
}

#[derive(Clone)]
struct KeyLayer {
    store: Store,
    operation: Arc<str>,
}

/// Key handling inside authentication: the verified caller and the request
/// budget, the key grammar, and the attempt for the handler's extractor.
/// After the handler, a success that never entered the seam is refused.
async fn handle_key(State(keys): State<KeyLayer>, request: Request, next: Next) -> Response {
    let request_id = request_id::request_id(request.extensions());
    let (mut parts, body) = request.into_parts();
    let Some(deadline) = parts
        .extensions
        .get::<RequestDeadline>()
        .map(RequestDeadline::at)
    else {
        return wiring_failure(&keys.operation, "deadline_missing", request_id);
    };
    let Ok(principal) = VerifiedPrincipal::from_request_parts(&mut parts, &()).await else {
        return wiring_failure(&keys.operation, "principal_missing", request_id);
    };
    let Some(caller) = Caller::of(principal.subject(), principal.client_id()) else {
        return wiring_failure(&keys.operation, "caller_missing", request_id);
    };
    let Some(key) = identity::valid_key(
        parts
            .headers
            .get_all(KEY_HEADER)
            .iter()
            .map(HeaderValue::as_bytes),
    ) else {
        Outcome::InvalidKey.record();
        return invalid_key(request_id);
    };
    let Some(scope) = identity::scope_key(principal.issuer(), caller, &keys.operation, key) else {
        return wiring_failure(&keys.operation, "scope_unencodable", request_id);
    };
    let seam_used = Arc::new(AtomicBool::new(false));
    let operation = Arc::clone(&keys.operation);
    parts.extensions.insert(Attempt {
        store: keys.store,
        scope,
        operation: keys.operation,
        deadline,
        request_id: request_id.clone(),
        seam_used: Arc::clone(&seam_used),
    });
    let response = next.run(Request::from_parts(parts, body)).await;
    guard_wiring(
        response,
        seam_used.load(Ordering::Relaxed),
        &operation,
        request_id,
    )
}

/// The 400 for an invalid key: a fixed detail and one `invalid_params` entry
/// that names the rule and never echoes the value.
fn invalid_key(request_id: Option<String>) -> Response {
    Problem::new(Code::BadRequest)
        .detail(INVALID_KEY_DETAIL)
        .invalid_param(format!("header.{KEY_HEADER}"), INVALID_KEY_REASON)
        .request_id(request_id)
        .into_response()
}

/// A handler that never entered the seam cannot return a success.
fn guard_wiring(
    response: Response,
    seam_used: bool,
    operation: &str,
    request_id: Option<String>,
) -> Response {
    if seam_used || !response.status().is_success() {
        return response;
    }
    wiring_failure(operation, "seam_unused", request_id)
}

fn wiring_failure(operation: &str, failure: &'static str, request_id: Option<String>) -> Response {
    tracing::error!(operation, failure, "http_idempotency_wiring_failed");
    sanitized(request_id)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER, WWW_AUTHENTICATE};
    use axum::http::{Method, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use utoipa_axum::routes;

    use super::*;
    use crate::idempotency::execute::recorded::Outcomes;
    use crate::idempotency::{
        Fingerprint, Idempotency, IdempotencyKey, IdempotentOperationProblemResponses, Tx,
    };
    use crate::problem::responses::ProblemComponents;
    use crate::{HardenOptions, harden};

    const WIDGETS: &str = "/_test/widgets";
    const VERSION: NonZeroU32 = NonZeroU32::MIN;

    #[utoipa::path(
        post,
        path = "/_test/widgets",
        operation_id = "infraHttpTestCreateWidget",
        params(IdempotencyKey),
        security(("bearerAuth" = [])),
        extensions(
            ("x-security-decision" = json!({
                "exposure": "protected",
                "rationale": "test-only route exercising the production idempotent composition"
            })),
            ("x-idempotent" = json!(true))
        ),
        responses(
            (status = 201, description = "created", content_type = "text/plain", body = String),
            IdempotentOperationProblemResponses,
        )
    )]
    async fn create_widget(idempotency: Idempotency) -> Response {
        idempotency
            .execute(
                Fingerprint::new(VERSION, "widget"),
                async |_: &mut Tx<'_>| (StatusCode::CREATED, "created"),
            )
            .await
    }

    #[utoipa::path(
        post,
        path = "/_test/undeclared",
        operation_id = "infraHttpTestUndeclared",
        params(IdempotencyKey),
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only route missing its idempotency declaration"
        }))),
        responses(
            (status = 201, description = "created", content_type = "text/plain", body = String),
            IdempotentOperationProblemResponses,
        )
    )]
    async fn undeclared() -> &'static str {
        "created"
    }

    #[utoipa::path(
        post,
        path = "/_test/unprotected",
        operation_id = "infraHttpTestUnprotected",
        params(IdempotencyKey),
        extensions(("x-idempotent" = json!(true))),
        responses(
            (status = 201, description = "created", content_type = "text/plain", body = String),
            IdempotentOperationProblemResponses,
        )
    )]
    async fn unprotected() -> &'static str {
        "created"
    }

    fn options() -> HardenOptions {
        HardenOptions {
            max_body_bytes: 1024,
            request_timeout: Duration::from_secs(1),
            max_in_flight: None,
            log_health_probes: false,
        }
    }

    fn served(routes: UtoipaMethodRouter) -> Router {
        let (routes, _) = OpenApiRouter::new().routes(routes).split_for_parts();
        harden(routes, &options())
    }

    fn request(method: Method, path: &str, headers: &[(&str, &str)]) -> Request {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn problem(response: Response) -> serde_json::Value {
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/problem+json"))
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn authentication_failures_precede_key_handling_and_fallbacks_stay() {
        let recorder = Outcomes::default();
        let _local = metrics::set_default_local_recorder(&recorder);
        let app = served(Composer::inert().route(routes!(create_widget)));
        let oversize = format!("Bearer {}", "a".repeat(32 * 1024 + 1));
        let cases = [
            (vec![], StatusCode::UNAUTHORIZED, "authentication_required"),
            (
                vec![
                    (AUTHORIZATION.as_str(), "Basic not-bearer"),
                    (KEY_HEADER, "\"k\""),
                ],
                StatusCode::BAD_REQUEST,
                "authentication_malformed",
            ),
            (
                vec![
                    (AUTHORIZATION.as_str(), oversize.as_str()),
                    (KEY_HEADER, "a b"),
                ],
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                "authentication_oversize",
            ),
            (
                vec![(AUTHORIZATION.as_str(), "Bearer opaque-token")],
                StatusCode::SERVICE_UNAVAILABLE,
                "authentication_unavailable",
            ),
        ];
        for (headers, status, code) in cases {
            let response = app
                .clone()
                .oneshot(request(Method::POST, WIDGETS, &headers))
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{code}");
            assert_eq!(
                response.headers().contains_key(WWW_AUTHENTICATE),
                status == StatusCode::UNAUTHORIZED,
                "{code}"
            );
            assert!(!response.headers().contains_key(RETRY_AFTER), "{code}");
            assert_eq!(problem(response).await["code"], code);
        }
        assert!(recorder.counts().is_empty(), "key handling never ran");

        let wrong_method = app
            .clone()
            .oneshot(request(Method::GET, WIDGETS, &[]))
            .await
            .unwrap();
        assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(wrong_method.headers()[ALLOW], "POST");
        assert_eq!(problem(wrong_method).await["code"], "method_not_allowed");
        let missing = app
            .oneshot(request(Method::POST, "/_test/missing", &[]))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(problem(missing).await["code"], "not_found");
    }

    #[tokio::test]
    async fn key_handling_without_a_verified_caller_is_a_wiring_fault() {
        let recorder = Outcomes::default();
        let _local = metrics::set_default_local_recorder(&recorder);
        let keys = KeyLayer {
            store: Store::inert(),
            operation: Arc::from("infraHttpTestCreateWidget"),
        };
        let unauthenticated: UtoipaMethodRouter = routes!(create_widget).map(|method_router| {
            method_router.route_layer(middleware::from_fn_with_state(keys, handle_key))
        });
        let response = served(unauthenticated)
            .oneshot(request(Method::POST, WIDGETS, &[(KEY_HEADER, "k-123")]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(problem(response).await["code"], "internal_error");
        assert!(recorder.counts().is_empty());
    }

    #[tokio::test]
    async fn a_tuple_that_breaks_a_rule_answers_500_and_agreement_refuses() {
        let cases: [(UtoipaMethodRouter, Rule); 2] = [
            (routes!(undeclared), Rule::Undeclared),
            (routes!(unprotected), Rule::Protected),
        ];
        for (routes, rule) in cases {
            let mut composer = Composer::inert();
            let contract = OpenApiRouter::<()>::with_openapi(ProblemComponents::openapi())
                .merge(composer.components())
                .routes(composer.route(routes));
            let path = contract
                .get_openapi()
                .paths
                .paths
                .keys()
                .next()
                .unwrap()
                .clone();
            let error = composer.agree(contract.get_openapi()).unwrap_err();
            assert_eq!(error.rule(), rule);

            let (routes, _) = contract.split_for_parts();
            let response = harden(routes, &options())
                .oneshot(request(Method::POST, &path, &[]))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(problem(response).await["code"], "internal_error");
        }
    }

    #[test]
    fn agreement_activates_only_with_a_composed_operation() {
        let composer = Composer::inert();
        let contract = OpenApiRouter::<()>::with_openapi(ProblemComponents::openapi())
            .merge(composer.components());
        assert!(matches!(
            composer.agree(contract.get_openapi()),
            Ok(Activation::Inactive)
        ));

        let mut composer = Composer::inert();
        let contract = OpenApiRouter::<()>::with_openapi(ProblemComponents::openapi())
            .merge(composer.components())
            .routes(composer.route(routes!(create_widget)));
        assert!(matches!(
            composer.agree(contract.get_openapi()),
            Ok(Activation::Active { operations, .. }) if operations.get() == 1
        ));
    }

    #[tokio::test]
    async fn an_invalid_key_is_a_400_that_never_echoes_the_value() {
        let response = invalid_key(Some("req-1".to_owned()));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            problem(response).await,
            serde_json::json!({
                "code": "bad_request",
                "type": "https://www.rfc-editor.org/rfc/rfc9110#section-15.5.1",
                "title": "bad request",
                "status": 400,
                "detail": "Idempotency-Key is missing or invalid",
                "request_id": "req-1",
                "invalid_params": [{
                    "name": "header.Idempotency-Key",
                    "reason": "must be one Idempotency-Key field of 1 to 255 RFC 9110 token characters"
                }]
            })
        );
    }

    #[tokio::test]
    async fn a_success_that_never_entered_the_seam_is_refused() {
        let refused = guard_wiring(
            (StatusCode::CREATED, "created").into_response(),
            false,
            "infraHttpTestCreateWidget",
            Some("req-1".to_owned()),
        );
        assert_eq!(refused.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let refused = problem(refused).await;
        assert_eq!(refused["code"], "internal_error");
        assert_eq!(refused["request_id"], "req-1");

        let executed = guard_wiring(
            StatusCode::CREATED.into_response(),
            true,
            "infraHttpTestCreateWidget",
            None,
        );
        assert_eq!(executed.status(), StatusCode::CREATED);
        let rejected = guard_wiring(
            StatusCode::FORBIDDEN.into_response(),
            false,
            "infraHttpTestCreateWidget",
            None,
        );
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }
}
