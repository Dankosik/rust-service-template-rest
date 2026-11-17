use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::Response;
use foldhash::{HashMap, HashMapExt as _};
use utoipa::openapi::security::{HttpAuthScheme, SecurityScheme};
use utoipa::openapi::{OpenApi, RefOr, path::Operation};
use utoipa_axum::router::OpenApiRouter;

use crate::HardenOptions;
use crate::probes::ProbeRoutes;
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::sanitized_internal_error;

mod outcomes;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FinalizeError {
    // template:begin http-idempotency:http-store-finalization-error
    #[error("mounted HTTP idempotency operations require an active idempotency store")]
    MissingIdempotencyStore,
    // template:end http-idempotency:http-store-finalization-error
    #[error("HTTP contract finalization failed: {method} {path} response {status}: {fault}")]
    InvalidResponse {
        method: String,
        path: String,
        status: String,
        fault: &'static str,
    },
    #[error(
        "HTTP contract finalization failed: {method} {path} is protected and requires enabled authentication"
    )]
    ProtectedOperation { path: String, method: String },
    #[error(
        "HTTP contract finalization failed: {method} {path} has an unsupported or ambiguous effective security policy ({fault})"
    )]
    InvalidPolicy {
        path: String,
        method: String,
        fault: &'static str,
    },
}

#[derive(Debug)]
pub enum Authentication {
    Public,
    // template:begin authn:http-assembly-verifier
    Bearer(Box<infra_bearerauthn::Verifier>),
    // template:end authn:http-assembly-verifier
}

#[derive(Clone)]
pub struct HttpContract<S> {
    routes: OpenApiRouter<S>,
    probes: Vec<Box<str>>,
    // template:begin http-idempotency:http-mounted-operations-field
    idempotency: MountedOperations,
    // template:end http-idempotency:http-mounted-operations-field
}

// template:begin http-idempotency:http-mounted-operations
#[derive(Clone, Default)]
pub(crate) struct MountedOperations(Vec<(Box<str>, &'static str)>);

impl MountedOperations {
    pub(crate) fn contains(&self, path: &str, method: &str) -> bool {
        self.0
            .iter()
            .any(|(registered, verb)| registered.as_ref() == path && *verb == method)
    }
}
// template:end http-idempotency:http-mounted-operations

fn nested_path(prefix: &str, path: &str) -> Box<str> {
    if prefix.ends_with('/') {
        format!("{prefix}{}", path.trim_start_matches('/')).into()
    } else if path.is_empty() || path == "/" {
        prefix.into()
    } else {
        format!("{prefix}{path}").into()
    }
}

impl<S: Clone + Send + Sync + 'static> Default for HttpContract<S> {
    fn default() -> Self {
        Self::new(OpenApiRouter::default())
    }
}

impl<S: Clone + Send + Sync + 'static> std::fmt::Debug for HttpContract<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpContract")
            .field("paths", &self.routes.get_openapi().paths.paths.len())
            .finish_non_exhaustive()
    }
}

impl<S: Clone + Send + Sync + 'static> HttpContract<S> {
    #[must_use]
    pub fn new(routes: OpenApiRouter<S>) -> Self {
        Self {
            routes,
            probes: Vec::new(),
            // template:begin http-idempotency:http-mounted-operations-init
            idempotency: MountedOperations::default(),
            // template:end http-idempotency:http-mounted-operations-init
        }
    }

    // template:begin http-idempotency:http-typed-registration
    pub(crate) fn from_idempotent(routes: OpenApiRouter<S>) -> Self {
        let mut idempotency = MountedOperations::default();
        for (path, item) in &routes.get_openapi().paths.paths {
            for (method, op) in self::fixed_path_operations!(item, as_ref) {
                if op.is_some() {
                    idempotency.0.push((path.as_str().into(), method));
                }
            }
        }
        Self {
            idempotency,
            ..Self::new(routes)
        }
    }

    #[must_use]
    pub fn idempotent_operations(&self) -> usize {
        self.idempotency.0.len()
    }
    // template:end http-idempotency:http-typed-registration

    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.routes = self.routes.merge(other.routes);
        self.probes.extend(other.probes);
        // template:begin http-idempotency:http-merge-mounted-operations
        self.idempotency.0.extend(other.idempotency.0);
        // template:end http-idempotency:http-merge-mounted-operations
        self
    }

    #[must_use]
    pub fn nest(mut self, prefix: &str, other: Self) -> Self {
        self.routes = self.routes.nest(prefix, other.routes);
        self.probes.extend(
            other
                .probes
                .into_iter()
                .map(|path| nested_path(prefix, &path)),
        );
        // template:begin http-idempotency:http-nest-mounted-operations
        self.idempotency.0.extend(
            other
                .idempotency
                .0
                .into_iter()
                .map(|(path, method)| (nested_path(prefix, &path), method)),
        );
        // template:end http-idempotency:http-nest-mounted-operations
        self
    }

    #[must_use]
    pub fn with_probes(mut self, prefix: &str) -> Self
    where
        health::ReadinessReader: axum::extract::FromRef<S>,
    {
        let probes = crate::probes::router();
        let mounted = if prefix.is_empty() || prefix == "/" {
            probes
        } else {
            OpenApiRouter::new().nest(prefix, probes)
        };
        self.probes.extend(
            mounted
                .get_openapi()
                .paths
                .paths
                .keys()
                .map(|path| path.as_str().into()),
        );
        self.routes = self.routes.merge(mounted);
        self
    }

    fn probe_routes(&self) -> ProbeRoutes {
        let paths = &self.routes.get_openapi().paths.paths;
        ProbeRoutes::new(
            self.probes
                .iter()
                .map(|path| {
                    let implicit_head = paths
                        .get(path.as_ref())
                        .is_some_and(|item| item.head.is_none());
                    (path.clone(), implicit_head)
                })
                .collect(),
        )
    }

    pub fn into_openapi(mut self) -> Result<OpenApi, FinalizeError> {
        self.finalize(&self.probe_routes())?;
        Ok(self.routes.into_openapi())
    }

    fn finalize(&mut self, probes: &ProbeRoutes) -> Result<Policy, FinalizeError> {
        let document = self.routes.get_openapi_mut();
        let policy = Policy::compile(
            document,
            // template:begin http-idempotency:http-policy-compile-mounts
            &self.idempotency,
            // template:end http-idempotency:http-policy-compile-mounts
        )?;
        outcomes::compose(
            document, probes,
            // template:begin authn:http-compose-policy
            &policy,
            // template:end authn:http-compose-policy
        )?;
        Ok(policy)
    }

    pub fn build(
        self,
        state: S,
        authentication: Authentication,
        options: &HardenOptions,
    ) -> Result<Application, FinalizeError> {
        self.build_with(
            state,
            authentication,
            options,
            // template:begin http-idempotency:http-build-no-store
            None,
            // template:end http-idempotency:http-build-no-store
        )
    }

    // template:begin http-idempotency:http-build-store
    pub fn build_with_idempotency_store(
        self,
        state: S,
        authentication: Authentication,
        options: &HardenOptions,
        store: infra_idempotency_store::Store,
    ) -> Result<Application, FinalizeError> {
        self.build_with(state, authentication, options, Some(store))
    }
    // template:end http-idempotency:http-build-store

    fn build_with(
        mut self,
        state: S,
        authentication: Authentication,
        options: &HardenOptions,
        // template:begin http-idempotency:http-build-store-parameter
        store: Option<infra_idempotency_store::Store>,
        // template:end http-idempotency:http-build-store-parameter
    ) -> Result<Application, FinalizeError> {
        let probes = self.probe_routes();
        let policy = self.finalize(&probes)?;
        let routes = match authentication {
            Authentication::Public => finalize_public(self.routes, policy)?,
            // template:begin authn:http-assembly-finalize
            Authentication::Bearer(auth) => crate::authn::finalize(self.routes, policy, *auth),
            // template:end authn:http-assembly-finalize
        };
        // template:begin http-idempotency:http-build-bind-store
        let routes = if self.idempotency.0.is_empty() {
            routes
        } else {
            routes.layer(axum::Extension(
                store.ok_or(FinalizeError::MissingIdempotencyStore)?,
            ))
        };
        // template:end http-idempotency:http-build-bind-store
        Ok(Application(crate::harden::assemble(
            routes.with_state(state),
            options,
            probes,
        )))
    }
}

#[derive(Clone, Debug)]
pub struct Application(Router);

impl Application {
    #[must_use]
    pub fn serve(
        self,
        listener: infra_http_transport::BoundListener,
        options: infra_http_transport::ServerOptions,
    ) -> infra_http_transport::Server {
        let router = self.0;
        #[cfg(feature = "hotpath")]
        let router = hotpath::axum!(router);
        listener.serve(router, options)
    }
}

impl tower::Service<Request> for Application {
    type Response = Response;
    type Error = Infallible;
    type Future = <Router as tower::Service<Request>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<Request>::poll_ready(&mut self.0, cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        tower::Service::call(&mut self.0, request)
    }
}

fn finalize_public<S>(
    contract: OpenApiRouter<S>,
    policy: Policy,
) -> Result<Router<S>, FinalizeError>
where
    S: Clone + Send + Sync + 'static,
{
    if let Some((path, method)) = policy.first_protected() {
        return Err(FinalizeError::ProtectedOperation {
            path: path.to_owned(),
            method: method.to_owned(),
        });
    }
    Ok(layer_routes(contract, |router| {
        router.route_layer(middleware::from_fn_with_state(policy, enforce_public))
    }))
}

pub(crate) fn layer_routes<S>(
    contract: OpenApiRouter<S>,
    layer: impl FnOnce(Router<S>) -> Router<S>,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let (router, _) = contract.split_for_parts();
    if router.has_routes() {
        layer(router)
    } else {
        router
    }
}

// OpenAPI 3.1 §4.8.30
pub(crate) enum Access {
    Public,
    Protected(
        // template:begin authn:http-contract-protected-scopes
        Box<[Box<[String]>]>,
        // template:end authn:http-contract-protected-scopes
    ),
}

fn compile_operation(
    document: &OpenApi,
    path: &str,
    method: &str,
    operation: &Operation,
    // template:begin http-idempotency:http-compile-operation-mounts
    mounted: &MountedOperations,
    // template:end http-idempotency:http-compile-operation-mounts
) -> Result<Access, FinalizeError> {
    let invalid = |fault| FinalizeError::InvalidPolicy {
        path: path.to_owned(),
        method: method.to_owned(),
        fault,
    };
    let access =
        Access::from_operation(document, operation).map_err(|fault| invalid(fault.as_str()))?;
    // template:begin http-idempotency:http-contract-idempotency-policy
    if mounted.contains(path, method) && matches!(access, Access::Public) {
        return Err(invalid("idempotency_requires_authentication"));
    }
    if !mounted.contains(path, method) && declares_idempotency(document, path, operation) {
        return Err(invalid("idempotency_requires_registration"));
    }
    // template:end http-idempotency:http-contract-idempotency-policy
    Ok(access)
}

// template:begin http-idempotency:http-unmounted-reserved-declarations
fn declares_idempotency(document: &OpenApi, path: &str, operation: &Operation) -> bool {
    use crate::idempotency::{KEY_HEADER, REPLAYED_HEADER};
    use utoipa::openapi::path::ParameterIn;
    let components = document.components.as_ref();
    let path_item = document.paths.paths.get(path).or_else(|| {
        (path == "/")
            .then(|| document.paths.paths.get(""))
            .flatten()
    });
    let key = path_item
        .and_then(|item| item.parameters.as_deref())
        .into_iter()
        .flatten()
        .chain(operation.parameters.iter().flatten())
        .filter_map(|value| match value {
            RefOr::T(parameter) => Some(parameter),
            RefOr::Ref(_) => {
                outcomes::resolve(value, &components?.parameters, "#/components/parameters/")
            }
        })
        .any(|parameter| {
            parameter.parameter_in == ParameterIn::Header
                && parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        });
    key || operation.responses.responses.iter().any(|(status, value)| {
        status.starts_with('2')
            && match value {
                RefOr::T(response) => Some(response),
                RefOr::Ref(_) => components.and_then(|components| {
                    outcomes::resolve(value, &components.responses, "#/components/responses/")
                }),
            }
            .is_some_and(|response| {
                response
                    .headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(REPLAYED_HEADER))
            })
    })
}
// template:end http-idempotency:http-unmounted-reserved-declarations

#[derive(Clone)]
pub(crate) struct Policy(Arc<CompiledPolicy>);

struct CompiledPolicy {
    routes: HashMap<Box<str>, HashMap<Box<str>, Access>>,
    first_protected: Option<(Box<str>, Box<str>)>,
}

macro_rules! fixed_path_operations {
    ($item:ident, $borrow:ident) => {
        [
            ("GET", $item.get.$borrow()),
            ("PUT", $item.put.$borrow()),
            ("POST", $item.post.$borrow()),
            ("DELETE", $item.delete.$borrow()),
            ("OPTIONS", $item.options.$borrow()),
            ("HEAD", $item.head.$borrow()),
            ("PATCH", $item.patch.$borrow()),
            ("TRACE", $item.trace.$borrow()),
            ("QUERY", $item.query.$borrow()),
        ]
    };
}

pub(super) use fixed_path_operations;

impl Policy {
    fn compile(
        document: &OpenApi,
        // template:begin http-idempotency:http-compile-mounts-parameter
        mounted: &MountedOperations,
        // template:end http-idempotency:http-compile-mounts-parameter
    ) -> Result<Self, FinalizeError> {
        let mut routes = HashMap::with_capacity(document.paths.paths.len());
        let mut first_protected = None;
        for (path, item) in &document.paths.paths {
            let path = if path.is_empty() {
                "/".to_owned()
            } else {
                path.clone()
            };
            let mut ordered_methods = BTreeMap::new();
            let fixed_operations = fixed_path_operations!(item, as_ref);
            for (method, operation) in fixed_operations {
                let Some(operation) = operation else {
                    continue;
                };
                ordered_methods.insert(
                    method.to_owned(),
                    compile_operation(
                        document, &path, method, operation,
                        // template:begin http-idempotency:http-fixed-operation-mounts
                        mounted,
                        // template:end http-idempotency:http-fixed-operation-mounts
                    )?,
                );
            }
            for (method, operation) in &item.additional_operations {
                if fixed_operations
                    .iter()
                    .any(|(fixed, _)| *fixed == method.as_str())
                {
                    return Err(FinalizeError::InvalidPolicy {
                        path,
                        method: method.clone(),
                        fault: "fixed_method_in_additional_operations",
                    });
                }
                ordered_methods.insert(
                    method.clone(),
                    compile_operation(
                        document, &path, method, operation,
                        // template:begin http-idempotency:http-additional-operation-mounts
                        mounted,
                        // template:end http-idempotency:http-additional-operation-mounts
                    )?,
                );
            }
            if first_protected.is_none()
                && let Some((method, _)) = ordered_methods
                    .iter()
                    .find(|(_, access)| matches!(access, Access::Protected(..)))
            {
                first_protected = Some((path.as_str().into(), method.as_str().into()));
            }
            let methods = ordered_methods
                .into_iter()
                .map(|(method, access)| (method.into_boxed_str(), access))
                .collect();
            routes.insert(path.into_boxed_str(), methods);
        }
        Ok(Self(Arc::new(CompiledPolicy {
            routes,
            first_protected,
        })))
    }

    fn first_protected(&self) -> Option<(&str, &str)> {
        self.0
            .first_protected
            .as_ref()
            .map(|(path, method)| (&**path, &**method))
    }

    pub(crate) fn access(&self, path: &str, method: &Method) -> Option<&Access> {
        let methods = self.0.routes.get(path)?;
        methods.get(method.as_str()).or_else(|| {
            if method == Method::HEAD {
                methods.get("GET")
            } else {
                None
            }
        })
    }
}

async fn enforce_public(State(policy): State<Policy>, request: Request, next: Next) -> Response {
    let route = contract_path(request.extensions());
    let public = route
        .and_then(|path| policy.access(path, request.method()))
        .is_some_and(|access| matches!(access, Access::Public));
    if public {
        next.run(request).await
    } else {
        tracing::error!(
            route,
            method = %request.method(),
            failure = "policy_absent",
            "http_contract_wiring_failed"
        );
        sanitized_internal_error()
    }
}

pub(crate) fn contract_path(extensions: &axum::http::Extensions) -> Option<&str> {
    Some(extensions.get::<axum::extract::MatchedPath>()?.as_str())
}

const BEARER_SCHEME: &str = "bearerAuth";

#[derive(Clone, Copy, Debug)]
enum PolicyFault {
    MissingBearerScheme,
    UnsupportedRequirement,
    UnreadableRequirement,
}

impl PolicyFault {
    const fn as_str(self) -> &'static str {
        match self {
            Self::MissingBearerScheme => "missing_bearer_scheme",
            Self::UnsupportedRequirement => "unsupported_requirement",
            Self::UnreadableRequirement => "unreadable_requirement",
        }
    }
}

impl Access {
    fn from_operation(document: &OpenApi, operation: &Operation) -> Result<Self, PolicyFault> {
        let security = operation.security.as_ref().or(document.security.as_ref());
        let Some(requirements) = security.filter(|requirements| !requirements.is_empty()) else {
            return Ok(Self::Public);
        };
        if !declares_bearer_scheme(document) {
            return Err(PolicyFault::MissingBearerScheme);
        }
        let requirements: Vec<BTreeMap<String, Vec<String>>> = serde_json::to_value(requirements)
            .and_then(serde_json::from_value)
            .map_err(|_| PolicyFault::UnreadableRequirement)?;
        if requirements
            .iter()
            .any(|requirement| requirement.len() != 1 || !requirement.contains_key(BEARER_SCHEME))
        {
            return Err(PolicyFault::UnsupportedRequirement);
        }
        Ok(Self::Protected(
            // template:begin authn:http-contract-protected-value
            requirements
                .into_iter()
                .flat_map(BTreeMap::into_values)
                .map(Vec::into_boxed_slice)
                .collect(),
            // template:end authn:http-contract-protected-value
        ))
    }
}

fn declares_bearer_scheme(document: &OpenApi) -> bool {
    document
        .components
        .as_ref()
        .and_then(|components| components.security_schemes.get(BEARER_SCHEME))
        .is_some_and(|scheme| {
            matches!(scheme, RefOr::T(SecurityScheme::Http(http)) if matches!(http.scheme, HttpAuthScheme::Bearer))
        })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::header::{ALLOW, CONTENT_TYPE};
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use utoipa::OpenApi;

    use super::*;
    use crate::test_support::body_json;

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

    fn app(contract: OpenApiRouter) -> Application {
        public_app(contract).unwrap()
    }

    fn public_app(contract: OpenApiRouter) -> Result<Application, FinalizeError> {
        HttpContract::new(contract).build(
            (),
            Authentication::Public,
            &HardenOptions {
                max_body_bytes: 1024,
                request_timeout: Duration::from_secs(1),
                max_in_flight: NonZeroU32::new(2),
                admission_wait: Duration::from_secs(1),
                log_health_probes: false,
                draining: None,
            },
        )
    }

    #[tokio::test]
    async fn public_query_and_additional_method_reach_the_registered_service() {
        let document = serde_json::from_value(serde_json::json!({
            "openapi": "3.2.0",
            "info": {"title": "method fixture", "version": "1"},
            "paths": {"/_test/method": {
                "query": {"responses": {"200": {"description": "ok"}, "503": {
                    "description": "index rebuilding",
                    "content": {"text/plain": {"schema": {"type": "string"}}}
                }}},
                "additionalOperations": {
                    "COPY": {"responses": {"200": {"description": "ok"}}}
                }
            }}
        }))
        .expect("valid OpenAPI fixture");
        let published = HttpContract::new(OpenApiRouter::<()>::with_openapi(document.clone()))
            .into_openapi()
            .unwrap();
        let published = serde_json::to_value(published).unwrap();
        let item = &published["paths"]["/_test/method"];
        let query_unavailable = &item["query"]["responses"]["503"];
        assert!(
            query_unavailable["description"]
                .as_str()
                .unwrap()
                .contains("index rebuilding")
        );
        assert_eq!(
            query_unavailable["content"]["text/plain"]["schema"]["type"],
            "string"
        );
        assert_eq!(
            query_unavailable["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
        assert!(query_unavailable["headers"]["Retry-After"].is_object());
        assert_eq!(
            item["additionalOperations"]["COPY"]["responses"]["503"]["$ref"],
            "#/components/responses/AdmissionUnavailable"
        );
        assert!(published["components"]["responses"]["AdmissionUnavailable"].is_object());
        let service = tower::service_fn(|request: axum::extract::Request| async move {
            Ok::<_, std::convert::Infallible>(request.method().as_str().to_owned())
        });
        #[expect(
            clippy::disallowed_methods,
            reason = "utoipa-axum 0.3 cannot register QUERY or extension methods with routes!"
        )]
        let app =
            app(OpenApiRouter::with_openapi(document).route_service("/_test/method", service));

        for method in ["QUERY", "COPY"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/_test/method")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{method}");
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                method.as_bytes()
            );
        }
    }

    // template:begin http-idempotency:http-unregistered-contract-test
    #[test]
    fn unregistered_operations_cannot_publish_idempotency_promises() {
        for (parameters, success) in [
            (
                serde_json::json!([{"in":"header","name":"idempotency-key","required":true}]),
                serde_json::json!({"description":"ok"}),
            ),
            (
                serde_json::json!([]),
                serde_json::json!({"$ref":"#/components/responses/Replay"}),
            ),
        ] {
            let document = serde_json::from_value(serde_json::json!({
                "openapi": "3.1.0", "info": {"title":"fixture","version":"1"},
                "components": {"responses": {"Replay": {"description": "ok", "headers": {"Idempotent-Replayed":{"schema":{"type":"string"}}}}}},
                "paths": {"/ordinary": {"parameters": parameters, "post": {"responses": {"201": success}}}}
            })).unwrap();
            let contract = HttpContract::new(OpenApiRouter::<()>::with_openapi(document));
            assert_eq!(
                contract.into_openapi().map(|_| ()),
                Err(FinalizeError::InvalidPolicy {
                    method: "POST".to_owned(),
                    path: "/ordinary".to_owned(),
                    fault: "idempotency_requires_registration"
                })
            );
        }
    }
    // template:end http-idempotency:http-unregistered-contract-test

    #[test]
    fn additional_operations_cannot_redeclare_fixed_methods() {
        let document = serde_json::from_value(serde_json::json!({
            "openapi": "3.2.0",
            "info": {"title": "method fixture", "version": "1"},
            "paths": {"/_test/method": {
                "additionalOperations": {
                    "POST": {"responses": {"200": {"description": "ok"}}}
                }
            }}
        }))
        .expect("OpenAPI fixture");
        assert_eq!(
            HttpContract::new(OpenApiRouter::<()>::with_openapi(document))
                .into_openapi()
                .map(|_| ()),
            Err(FinalizeError::InvalidPolicy {
                path: "/_test/method".to_owned(),
                method: "POST".to_owned(),
                fault: "fixed_method_in_additional_operations",
            })
        );
    }

    fn protected() -> FinalizeError {
        FinalizeError::ProtectedOperation {
            path: "/_test/policy".to_owned(),
            method: "GET".to_owned(),
        }
    }

    fn invalid(fault: &'static str) -> FinalizeError {
        FinalizeError::InvalidPolicy {
            path: "/_test/policy".to_owned(),
            method: "GET".to_owned(),
            fault,
        }
    }

    #[test]
    fn public_finalization_checks_security_but_leaves_documentation_to_the_gate() {
        for (root, security, exposure, expected) in [
            (None, None, None, Ok(())),
            (None, Some(serde_json::json!([])), None, Ok(())),
            (
                Some(serde_json::json!([{"bearerAuth": []}])),
                Some(serde_json::json!([])),
                None,
                Ok(()),
            ),
            (
                Some(serde_json::json!([{"bearerAuth": []}])),
                None,
                None,
                Err(protected()),
            ),
            (None, Some(serde_json::json!([])), Some("protected"), Ok(())),
            (None, Some(serde_json::json!([])), Some("public"), Ok(())),
            (
                None,
                Some(serde_json::json!([{}])),
                None,
                Err(invalid("unsupported_requirement")),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": ["write"]}])),
                None,
                Err(protected()),
            ),
            (
                None,
                Some(serde_json::json!([{"unknown": []}])),
                None,
                Err(invalid("unsupported_requirement")),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": []}, {}])),
                None,
                Err(invalid("unsupported_requirement")),
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
            let contract = OpenApiRouter::<()>::with_openapi(
                serde_json::from_value(document.clone()).expect("valid OpenAPI fixture"),
            );
            assert_eq!(
                public_app(contract.clone()).map(|_| ()),
                expected,
                "{document}"
            );
            let export = HttpContract::new(contract).into_openapi().map(|_| ());
            match &expected {
                Err(FinalizeError::ProtectedOperation { .. }) => assert_eq!(export, Ok(())),
                _ => assert_eq!(export, expected, "{document}"),
            }
        }
    }

    #[tokio::test]
    async fn implicit_head_inherits_get_but_native_method_fallback_remains() {
        let app = app(OpenApiRouter::with_openapi(Api::openapi())
            .routes(utoipa_axum::routes!(documented_get)));
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
    async fn nested_contract_keeps_its_policy() {
        for prefix in ["/mounted", "/mounted/"] {
            assert_nested_contract_serves(prefix).await;
        }
    }

    async fn assert_nested_contract_serves(prefix: &str) {
        let app = app(OpenApiRouter::new().nest(
            prefix,
            OpenApiRouter::with_openapi(Api::openapi())
                .routes(utoipa_axum::routes!(documented_get)),
        ));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/mounted/_test/method")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK, "nested at {prefix}");
    }

    #[tokio::test]
    async fn served_operation_missing_policy_is_sanitized() {
        let (router, document) = OpenApiRouter::with_openapi(Api::openapi())
            .routes(utoipa_axum::routes!(documented_get))
            .split_for_parts();
        let handler_1 = || async { "must not run" };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let mut contract =
            OpenApiRouter::from(router.route("/undocumented", axum::routing::post(handler_1)));
        *contract.get_openapi_mut() = document;
        let response = app(contract)
            .oneshot(
                Request::post("/undocumented")
                    .header("x-request-id", "missing-policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        assert_eq!(response.headers()["x-request-id"], "missing-policy");
        let problem = body_json(response).await;
        assert_eq!(problem["code"], "internal_error");
        assert_eq!(problem["detail"], SANITIZED_DETAIL);
    }
}
