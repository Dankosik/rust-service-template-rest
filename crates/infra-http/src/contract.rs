//! The served-route contract and the actual methods that constructed it.
//!
//! `utoipa_axum::routes!` owns annotation and schema extraction. This module
//! deliberately rebuilds the axum method router from that metadata and the
//! supplied handler, so the served endpoints and the OpenAPI document have one
//! provenance carrier. In particular, an opaque tuple cannot add an explicit
//! `HEAD` endpoint that the final document does not describe.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use axum::Router;
use axum::extract::{Request, State};
use axum::handler::Handler;
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter};
use utoipa::openapi::OpenApi;
use utoipa::openapi::path::{Operation, PathItem, Paths};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use crate::problem::{Code, Problem, SANITIZED_DETAIL};
use crate::request_id;

/// The complete inbound contract before its one serving finalization.
///
/// The inner axum and Utoipa values stay private. Consumers may merge only
/// tracked registrations or a component document, render the document, or
/// consume the carrier through one supported finalizer.
pub struct ContractRouter<S = ()> {
    router: OpenApiRouter<S>,
    registered: RouteMethods,
}

impl<S> fmt::Debug for ContractRouter<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContractRouter")
            .field("registered", &self.registered)
            .finish_non_exhaustive()
    }
}

impl<S> ContractRouter<S>
where
    S: Send + Sync + Clone + 'static,
{
    /// Begin a contract with service-level OpenAPI metadata or components.
    #[must_use]
    pub fn with_openapi(openapi: OpenApi) -> Self {
        Self {
            router: OpenApiRouter::with_openapi(openapi),
            registered: RouteMethods::default(),
        }
    }

    /// Merge an independently constructed tracked carrier.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.router = self.router.merge(other.router);
        self.registered.merge(other.registered);
        self
    }

    /// Merge a document that contributes components but no served routes.
    #[must_use]
    pub fn merge_document(self, document: OpenApi) -> Self {
        self.merge(Self::with_openapi(document))
    }

    /// Register documented or explicitly undocumented tracked routes.
    #[must_use]
    pub fn routes(mut self, registered: RegisteredRoutes<S>) -> Self {
        let RegisteredRoutes {
            registrations,
            methods,
        } = registered;
        for registration in registrations {
            self.router = match registration {
                Registration::Documented(routes) => self.router.routes(routes),
                Registration::Undocumented { path, router } => self.router.route(&path, router),
            };
        }
        self.registered.merge(methods);
        self
    }

    /// Read the one document the carrier has assembled.
    #[must_use]
    pub fn document(&self) -> &OpenApi {
        self.router.get_openapi()
    }

    /// Consume this carrier for documentation generation only.
    #[must_use]
    pub fn into_document(self) -> OpenApi {
        self.router.into_openapi()
    }

    /// Supply application state after policy finalization.
    #[must_use]
    pub fn with_state<S2>(self, state: S) -> ContractRouter<S2>
    where
        S2: Send + Sync + Clone + 'static,
    {
        ContractRouter {
            router: self.router.with_state(state),
            registered: self.registered,
        }
    }

    /// Finalize a public-only generated contract without an auth-provider
    /// dependency. Protected or contradictory operations are refused; a real
    /// endpoint missing from the document answers a sanitized 500.
    ///
    /// # Errors
    ///
    /// Returns a closed composition error for a non-public security policy.
    pub fn finalize_public(self) -> Result<Router<S>, FinalizeError> {
        let policy = PublicPolicy::compile(self.router.get_openapi(), &self.registered)?;
        let (router, _) = self.router.split_for_parts();
        if self.registered.is_empty() {
            Ok(router)
        } else {
            Ok(router.route_layer(middleware::from_fn_with_state(policy, enforce_public)))
        }
    }

    // template:begin authn:contract-authn-parts
    pub(crate) fn into_parts(self) -> (OpenApiRouter<S>, RouteMethods) {
        (self.router, self.registered)
    }
    // template:end authn:contract-authn-parts
}

/// A group of endpoints whose exact methods are tracked while it is composed.
///
/// It intentionally has no accessor for the underlying method routers. The
/// idempotency boundary may apply a method-preserving layer through its
/// crate-private seam, then return the same carrier for final composition.
pub struct RegisteredRoutes<S = ()> {
    registrations: Vec<Registration<S>>,
    methods: RouteMethods,
}

impl<S> fmt::Debug for RegisteredRoutes<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredRoutes")
            .field("registrations", &self.registrations.len())
            .field("methods", &self.methods)
            .finish()
    }
}

impl<S> RegisteredRoutes<S>
where
    S: Send + Sync + Clone + 'static,
{
    /// Bind one annotated handler to the actual methods its annotation
    /// declares, discarding the opaque method router from Utoipa.
    #[must_use]
    pub fn documented<T, H>(handler: H, annotated: UtoipaMethodRouter<S>) -> Self
    where
        H: Handler<T, S> + Clone + Send + Sync + 'static,
        T: 'static,
    {
        let (schemas, paths, _) = annotated;
        let methods = RouteMethods::from_paths(&paths);
        let router = methods.method_router(handler);
        Self {
            registrations: vec![Registration::Documented((schemas, paths, router))],
            methods,
        }
    }

    /// Register one real endpoint with no OpenAPI operation metadata.
    ///
    /// Unless another carrier supplies its matching operation, the finalized
    /// router answers a sanitized 500 without invoking this handler.
    #[must_use]
    pub fn undocumented<T, H>(path: impl Into<String>, method: RouteMethod, handler: H) -> Self
    where
        H: Handler<T, S> + Clone + Send + Sync + 'static,
        T: 'static,
    {
        let path = normalize_path(&path.into());
        let router = MethodRouter::new().on(method.filter(), handler);
        Self {
            registrations: vec![Registration::Undocumented {
                path: path.clone(),
                router,
            }],
            methods: RouteMethods::single(path, method),
        }
    }

    /// Combine independently declared route groups while retaining all method
    /// provenance.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.registrations.extend(other.registrations);
        self.methods.merge(other.methods);
        self
    }

    // template:begin http-idempotency:contract-idempotency-route-methods
    pub(crate) fn documented_paths(&self) -> Option<&Paths> {
        match self.registrations.as_slice() {
            [Registration::Documented((_, paths, _))] => Some(paths),
            _ => None,
        }
    }

    pub(crate) fn map_method_routers(
        self,
        mut map: impl FnMut(MethodRouter<S>) -> MethodRouter<S>,
    ) -> Self {
        let registrations = self
            .registrations
            .into_iter()
            .map(|registration| match registration {
                Registration::Documented((schemas, paths, router)) => {
                    Registration::Documented((schemas, paths, map(router)))
                }
                Registration::Undocumented { path, router } => Registration::Undocumented {
                    path,
                    router: map(router),
                },
            })
            .collect();
        Self {
            registrations,
            methods: self.methods,
        }
    }
    // template:end http-idempotency:contract-idempotency-route-methods
}

enum Registration<S> {
    Documented(UtoipaMethodRouter<S>),
    Undocumented {
        path: String,
        router: MethodRouter<S>,
    },
}

/// A registered HTTP method. This is the only method vocabulary OpenAPI and
/// axum share for this inbound contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RouteMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

impl RouteMethod {
    const ALL: [Self; 8] = [
        Self::Get,
        Self::Put,
        Self::Post,
        Self::Delete,
        Self::Options,
        Self::Head,
        Self::Patch,
        Self::Trace,
    ];

    pub(crate) fn from_request(method: &Method) -> Option<Self> {
        if method == Method::GET {
            Some(Self::Get)
        } else if method == Method::PUT {
            Some(Self::Put)
        } else if method == Method::POST {
            Some(Self::Post)
        } else if method == Method::DELETE {
            Some(Self::Delete)
        } else if method == Method::OPTIONS {
            Some(Self::Options)
        } else if method == Method::HEAD {
            Some(Self::Head)
        } else if method == Method::PATCH {
            Some(Self::Patch)
        } else if method == Method::TRACE {
            Some(Self::Trace)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Put => "put",
            Self::Post => "post",
            Self::Delete => "delete",
            Self::Options => "options",
            Self::Head => "head",
            Self::Patch => "patch",
            Self::Trace => "trace",
        }
    }

    const fn filter(self) -> MethodFilter {
        match self {
            Self::Get => MethodFilter::GET,
            Self::Put => MethodFilter::PUT,
            Self::Post => MethodFilter::POST,
            Self::Delete => MethodFilter::DELETE,
            Self::Options => MethodFilter::OPTIONS,
            Self::Head => MethodFilter::HEAD,
            Self::Patch => MethodFilter::PATCH,
            Self::Trace => MethodFilter::TRACE,
        }
    }

    fn present(self, item: &PathItem) -> bool {
        match self {
            Self::Get => item.get.is_some(),
            Self::Put => item.put.is_some(),
            Self::Post => item.post.is_some(),
            Self::Delete => item.delete.is_some(),
            Self::Options => item.options.is_some(),
            Self::Head => item.head.is_some(),
            Self::Patch => item.patch.is_some(),
            Self::Trace => item.trace.is_some(),
        }
    }

    pub(crate) fn operation<'a>(self, item: &'a PathItem) -> Option<&'a Operation> {
        match self {
            Self::Get => item.get.as_ref(),
            Self::Put => item.put.as_ref(),
            Self::Post => item.post.as_ref(),
            Self::Delete => item.delete.as_ref(),
            Self::Options => item.options.as_ref(),
            Self::Head => item.head.as_ref(),
            Self::Patch => item.patch.as_ref(),
            Self::Trace => item.trace.as_ref(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RouteMethods(BTreeMap<String, BTreeSet<RouteMethod>>);

impl RouteMethods {
    fn single(path: String, method: RouteMethod) -> Self {
        let mut methods = BTreeMap::new();
        methods.insert(path, BTreeSet::from([method]));
        Self(methods)
    }

    fn from_paths(paths: &Paths) -> Self {
        let mut methods = Self::default();
        for (path, item) in &paths.paths {
            let registered = methods.0.entry(normalize_path(path)).or_default();
            registered.extend(
                RouteMethod::ALL
                    .into_iter()
                    .filter(|method| method.present(item)),
            );
        }
        methods
    }

    fn merge(&mut self, other: Self) {
        for (path, methods) in other.0 {
            self.0.entry(path).or_default().extend(methods);
        }
    }

    fn method_router<S, T, H>(&self, handler: H) -> MethodRouter<S>
    where
        H: Handler<T, S> + Clone + Send + Sync + 'static,
        T: 'static,
        S: Send + Sync + Clone + 'static,
    {
        self.0
            .values()
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .fold(MethodRouter::new(), |router, method| {
                router.on(method.filter(), handler.clone())
            })
    }

    pub(crate) fn effective(&self, path: &str, request_method: &Method) -> Option<RouteMethod> {
        let requested = RouteMethod::from_request(request_method)?;
        let methods = self.0.get(path)?;
        if methods.contains(&requested) {
            Some(requested)
        } else if requested == RouteMethod::Head && methods.contains(&RouteMethod::Get) {
            Some(RouteMethod::Get)
        } else {
            None
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, RouteMethod)> {
        self.0.iter().flat_map(|(path, methods)| {
            methods
                .iter()
                .copied()
                .map(move |method| (path.as_str(), method))
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A closed finalization error. Detailed policy diagnostics stay in the
/// composition logs; callers never receive a partially served contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("HTTP contract finalization failed")]
pub enum FinalizeError {
    /// A protected operation requires enabled authentication.
    NonPublicOperation,
    /// An operation's security policy is unsupported or contradictory.
    InvalidPolicy,
}

#[derive(Clone)]
struct PublicPolicy {
    methods: RouteMethods,
    missing: BTreeSet<(String, RouteMethod)>,
}

impl PublicPolicy {
    fn compile(document: &OpenApi, methods: &RouteMethods) -> Result<Self, FinalizeError> {
        let mut missing = BTreeSet::new();
        for (path, method) in methods.iter() {
            let Some(operation) = document
                .paths
                .paths
                .get(path)
                .and_then(|item| method.operation(item))
            else {
                missing.insert((path.to_owned(), method));
                continue;
            };
            if !is_public(document, operation)? {
                return Err(FinalizeError::NonPublicOperation);
            }
        }
        Ok(Self {
            methods: methods.clone(),
            missing,
        })
    }

    fn is_missing(&self, request: &Request) -> bool {
        let Some(path) = request
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map(axum::extract::MatchedPath::as_str)
        else {
            return true;
        };
        self.methods
            .effective(path, request.method())
            .is_none_or(|method| self.missing.contains(&(path.to_owned(), method)))
    }
}

async fn enforce_public(
    State(policy): State<PublicPolicy>,
    request: Request,
    next: Next,
) -> Response {
    if policy.is_missing(&request) {
        Problem::new(Code::InternalServerError)
            .detail(SANITIZED_DETAIL)
            .request_id(request_id::request_id(request.extensions()))
            .into_response()
    } else {
        next.run(request).await
    }
}

pub(crate) fn is_public(document: &OpenApi, operation: &Operation) -> Result<bool, FinalizeError> {
    let security = operation.security.as_ref().or(document.security.as_ref());
    let public = security.is_none_or(Vec::is_empty);
    if let Some(requirements) = security.filter(|_| !public) {
        let value = serde_json::to_value(requirements).map_err(|_| FinalizeError::InvalidPolicy)?;
        let bearer_only = value.as_array().is_some_and(|requirements| {
            requirements.iter().all(|requirement| {
                requirement.as_object().is_some_and(|requirement| {
                    requirement.len() == 1
                        && requirement.get("bearerAuth")
                            == Some(&serde_json::Value::Array(Vec::new()))
                })
            })
        });
        if !bearer_only {
            return Err(FinalizeError::InvalidPolicy);
        }
    }
    if let Some(decision) = operation
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("x-security-decision"))
    {
        let expected = if public { "public" } else { "protected" };
        if decision.get("exposure").and_then(serde_json::Value::as_str) != Some(expected) {
            return Err(FinalizeError::InvalidPolicy);
        }
    }
    Ok(public)
}

fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_owned()
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::header::{ALLOW, CONTENT_TYPE};
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use utoipa::OpenApi;

    use super::*;
    use crate::{HardenOptions, harden};

    #[derive(OpenApi)]
    struct Api;

    #[utoipa::path(
        get,
        path = "/_test/method",
        operation_id = "infraHttpContractDocumentedGet",
        security(),
        extensions(("x-security-decision" = json!({
            "exposure": "public",
            "rationale": "contract provenance test route"
        }))),
        responses((status = 200, description = "ok", content_type = "text/plain", body = String))
    )]
    async fn documented_get() -> &'static str {
        "get"
    }

    fn app(contract: ContractRouter) -> axum::Router {
        harden(
            contract
                .finalize_public()
                .expect("the documented public contract finalizes"),
            &HardenOptions {
                max_body_bytes: 1024,
                request_timeout: Duration::from_secs(1),
                max_in_flight: NonZeroU32::new(2),
                log_health_probes: false,
            },
        )
    }

    #[test]
    fn repair_regression_public_policy_uses_effective_security_and_optional_context() {
        for (root, security, exposure, public) in [
            (None, None, None, true),
            (None, Some(serde_json::json!([])), None, true),
            (
                Some(serde_json::json!([{"bearerAuth": []}])),
                Some(serde_json::json!([])),
                None,
                true,
            ),
            (
                Some(serde_json::json!([{"bearerAuth": []}])),
                None,
                None,
                false,
            ),
            (None, Some(serde_json::json!([])), Some("protected"), false),
            (None, Some(serde_json::json!([])), Some("public"), true),
            (None, Some(serde_json::json!([{}])), None, false),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": ["write"]}])),
                None,
                false,
            ),
            (
                None,
                Some(serde_json::json!([{"unknown": []}])),
                None,
                false,
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": []}, {}])),
                None,
                false,
            ),
        ] {
            let mut document = serde_json::json!({
                "openapi": "3.1.0",
                "info": {"title": "policy fixture", "version": "1"},
                "components": {"securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}}},
                "paths": {"/_test/policy": {"get": {"responses": {"200": {"description": "ok"}}}}}
            });
            if let Some(root) = root {
                document["security"] = root;
            }
            if let Some(security) = security {
                document["paths"]["/_test/policy"]["get"]["security"] = security;
            }
            if let Some(exposure) = exposure {
                document["paths"]["/_test/policy"]["get"]["x-security-decision"] =
                    serde_json::json!({"exposure": exposure});
            }
            let contract = ContractRouter::<()>::with_openapi(
                serde_json::from_value(document.clone()).expect("valid OpenAPI fixture"),
            )
            .routes(RegisteredRoutes::undocumented(
                "/_test/policy",
                RouteMethod::Get,
                || async { "ok" },
            ));
            assert_eq!(contract.finalize_public().is_ok(), public, "{document}");
        }
    }

    #[tokio::test]
    async fn implicit_head_inherits_get_but_native_method_fallback_remains() {
        let app = app(
            ContractRouter::with_openapi(Api::openapi()).routes(crate::routes!(documented_get))
        );
        let head = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/_test/method")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("the HEAD response");
        assert_eq!(head.status(), StatusCode::OK);
        assert!(
            head.into_body()
                .collect()
                .await
                .expect("a complete body")
                .to_bytes()
                .is_empty()
        );

        let post = app
            .oneshot(Request::post("/_test/method").body(Body::empty()).unwrap())
            .await
            .expect("the method fallback response");
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            post.headers()
                .get(ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("GET,HEAD")
        );
        assert_eq!(
            post.headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
    }

    #[tokio::test]
    async fn explicit_undocumented_head_is_sanitized_instead_of_inheriting_get() {
        let app = app(ContractRouter::with_openapi(Api::openapi())
            .routes(crate::routes!(documented_get))
            .routes(RegisteredRoutes::undocumented(
                "/_test/method",
                RouteMethod::Head,
                || async { "undocumented" },
            )));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/_test/method")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("the explicit HEAD response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
    }
}
