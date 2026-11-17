use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::extract::{Request, State};
use axum::handler::Handler;
use axum::http::{HeaderValue, Method, header::ALLOW};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{MethodFilter, MethodRouter};
use foldhash::{HashMap, HashMapExt as _};
use itertools::Itertools as _;
use tower::ServiceExt as _;
use utoipa::openapi::path::{Operation, PathItem, Paths};
use utoipa::openapi::security::{HttpAuthScheme, SecurityScheme};
use utoipa::openapi::{Components, OpenApi, RefOr};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use crate::HardenOptions;
use crate::probes::ProbeRoutes;
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::sanitized_internal_error;

mod components;
mod outcomes;

#[macro_export]
macro_rules! http_route {
    ($($handler:ident)::+) => {{
        $crate::HttpContract::from_annotation($crate::annotated_routes!($($handler)::+), $($handler)::+)
    }};
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FinalizeError {
    #[error("application metadata must contain no registered paths")]
    ApplicationPaths,
    #[error("only the application root may supply global security, schemes, or metadata")]
    ApplicationMetadata,
    #[error("an annotated registration requires one concrete path with native HTTP operations")]
    InvalidRegistration,
    #[error("{method} must be registered as an annotated HTTP operation")]
    NativeMethod { method: String },
    // template:begin http-idempotency:http-store-finalization-error
    #[error("mounted HTTP idempotency operations require an active idempotency store")]
    MissingIdempotencyStore,
    // template:end http-idempotency:http-store-finalization-error
    #[error(
        "HTTP contract finalization failed: components.{kind}.{name} conflicts with the shared HTTP contract"
    )]
    ConflictingComponent { kind: &'static str, name: String },
    #[error("HTTP contract composition defines {method} {path} more than once")]
    DuplicateOperation { path: String, method: String },
    #[error("HTTP contract composition cannot merge {path} with different path-level {field}")]
    ConflictingPathItem { path: String, field: &'static str },
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
    application: bool,
    extensions: BTreeMap<String, HashMap<Method, MethodRouter<S>>>,
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
        Self {
            routes: OpenApiRouter::default(),
            application: false,
            extensions: BTreeMap::new(),
            probes: Vec::new(),
            // template:begin http-idempotency:http-mounted-operations-init
            idempotency: MountedOperations::default(),
            // template:end http-idempotency:http-mounted-operations-init
        }
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
    pub fn application(metadata: OpenApi) -> Result<Self, FinalizeError> {
        if !metadata.paths.paths.is_empty() {
            return Err(FinalizeError::ApplicationPaths);
        }
        Ok(Self {
            routes: OpenApiRouter::with_openapi(metadata),
            application: true,
            ..Self::default()
        })
    }

    pub fn with_components(mut self, incoming: Components) -> Result<Self, FinalizeError> {
        if !incoming.security_schemes.is_empty() {
            return Err(FinalizeError::ApplicationMetadata);
        }
        components::merge(
            self.routes
                .get_openapi_mut()
                .components
                .get_or_insert_default(),
            incoming,
        )?;
        Ok(self)
    }

    #[doc(hidden)]
    pub fn from_annotation<H, T>(
        (schemas, paths, _): UtoipaMethodRouter<S>,
        handler: H,
    ) -> Result<Self, FinalizeError>
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let mut document = OpenApi::default();
        let components = document.components.get_or_insert_default();
        components::merge_map("schemas", &mut components.schemas, schemas)?;
        document.paths = paths;
        Self::annotated(document, handler)
    }

    fn annotated<H, T>(mut document: OpenApi, handler: H) -> Result<Self, FinalizeError>
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let paths = std::mem::take(&mut document.paths);
        let item = paths
            .paths
            .values()
            .exactly_one()
            .map_err(|_| FinalizeError::InvalidRegistration)?;
        if item.ref_path.is_some() || !item.additional_operations.is_empty() {
            return Err(FinalizeError::InvalidRegistration);
        }
        let method = documented_methods(item)
            .exactly_one()
            .map_err(|_| FinalizeError::InvalidRegistration)?;
        let filter = Method::from_bytes(method.as_bytes())
            .ok()
            .and_then(|method| MethodFilter::try_from(method).ok())
            .ok_or(FinalizeError::InvalidRegistration)?;
        Self::feature_route(
            document,
            (Vec::new(), paths, axum::routing::on(filter, handler)),
        )
    }

    fn feature_route(
        metadata: OpenApi,
        route: UtoipaMethodRouter<S>,
    ) -> Result<Self, FinalizeError> {
        if metadata.security.is_some()
            || !metadata.paths.paths.is_empty()
            || metadata.servers.is_some()
            || metadata.tags.is_some()
            || metadata.webhooks.is_some()
            || metadata.external_docs.is_some()
            || metadata.extensions.is_some()
            || metadata.json_schema_dialect.is_some()
            || metadata.self_uri.is_some()
        {
            return Err(FinalizeError::ApplicationMetadata);
        }
        let mut contract =
            Self::default().with_components(metadata.components.unwrap_or_default())?;
        contract.routes = contract.routes.routes(route);
        normalize_root(&mut contract.routes.get_openapi_mut().paths)?;
        Ok(contract)
    }

    pub fn extension<H, T>(
        self,
        path: &str,
        method: Method,
        operation: Operation,
        handler: H,
    ) -> Result<Self, FinalizeError>
    where
        H: Handler<T, S>,
        T: 'static,
    {
        if MethodFilter::try_from(method.clone()).is_ok() {
            return Err(FinalizeError::NativeMethod {
                method: method.to_string(),
            });
        }
        let path = if path.is_empty() { "/" } else { path };
        let mut item = PathItem::default();
        if method.as_str() == "QUERY" {
            item.query = Some(operation);
        } else {
            item.additional_operations
                .insert(method.to_string(), operation);
        }
        let mut feature = Self::default();
        feature
            .routes
            .get_openapi_mut()
            .paths
            .paths
            .insert(path.to_owned(), item);
        #[expect(
            clippy::disallowed_methods,
            reason = "the dispatcher selects this handler by its exact registered extension method"
        )]
        let handler = axum::routing::any(handler);
        feature
            .extensions
            .entry(path.to_owned())
            .or_default()
            .insert(method, handler);
        self.merge(feature)
    }

    pub fn with_state<S2: Clone + Send + Sync + 'static>(self, state: S) -> HttpContract<S2> {
        HttpContract {
            routes: self.routes.with_state(state.clone()),
            extensions: self
                .extensions
                .into_iter()
                .map(|(path, handlers)| {
                    (
                        path,
                        handlers
                            .into_iter()
                            .map(|(method, handler)| (method, handler.with_state(state.clone())))
                            .collect(),
                    )
                })
                .collect(),
            probes: self.probes,
            application: self.application,
            // template:begin http-idempotency:http-state-mounted-operations
            idempotency: self.idempotency,
            // template:end http-idempotency:http-state-mounted-operations
        }
    }

    // template:begin http-idempotency:http-typed-registration
    pub(crate) fn from_idempotent(
        metadata: OpenApi,
        route: UtoipaMethodRouter<S>,
    ) -> Result<Self, FinalizeError> {
        let mut contract = Self::feature_route(metadata, route)?;
        for (path, item) in &contract.routes.get_openapi().paths.paths {
            for (method, op) in self::fixed_path_operations!(item, as_ref) {
                if op.is_some() {
                    let path = if path.is_empty() { "/" } else { path.as_str() };
                    contract.idempotency.0.push((path.into(), method));
                }
            }
        }
        Ok(contract)
    }

    #[must_use]
    pub fn idempotent_operations(&self) -> usize {
        self.idempotency.0.len()
    }
    // template:end http-idempotency:http-typed-registration

    pub fn merge(mut self, mut other: Self) -> Result<Self, FinalizeError> {
        if other.application {
            return Err(FinalizeError::ApplicationMetadata);
        }
        normalize_root(&mut self.routes.get_openapi_mut().paths)?;
        normalize_root(&mut other.routes.get_openapi_mut().paths)?;
        let parent = &self.routes.get_openapi().paths.paths;
        for (path, incoming) in &other.routes.get_openapi().paths.paths {
            if let Some(existing) = parent.get(path) {
                check_path_merge(path, existing, incoming)?;
            }
        }
        if let Some(incoming) = other.routes.get_openapi_mut().components.take() {
            self = self.with_components(incoming)?;
        }
        self.routes = self.routes.merge(other.routes);
        for (path, handlers) in other.extensions {
            self.extensions.entry(path).or_default().extend(handlers);
        }
        self.probes.extend(other.probes);
        // template:begin http-idempotency:http-merge-mounted-operations
        self.idempotency.0.extend(other.idempotency.0);
        // template:end http-idempotency:http-merge-mounted-operations
        Ok(self)
    }

    pub fn nest(self, prefix: &str, mut other: Self) -> Result<Self, FinalizeError> {
        if other.application {
            return Err(FinalizeError::ApplicationMetadata);
        }
        normalize_root(&mut other.routes.get_openapi_mut().paths)?;
        let components = other.routes.get_openapi_mut().components.take();
        other.routes = OpenApiRouter::default().nest(prefix, other.routes);
        other.routes.get_openapi_mut().components = components;
        other.extensions = other
            .extensions
            .into_iter()
            .map(|(path, handlers)| (nested_path(prefix, &path).into_string(), handlers))
            .collect();
        for path in &mut other.probes {
            *path = nested_path(prefix, path);
        }
        // template:begin http-idempotency:http-nest-mounted-operations
        for (path, _) in &mut other.idempotency.0 {
            *path = nested_path(prefix, path);
        }
        // template:end http-idempotency:http-nest-mounted-operations
        self.merge(other)
    }

    pub fn with_probes(self, prefix: &str) -> Result<Self, FinalizeError>
    where
        health::ReadinessReader: axum::extract::FromRef<S>,
    {
        let mut probes = crate::probes::router()?;
        probes.probes = probes
            .routes
            .get_openapi()
            .paths
            .paths
            .keys()
            .map(|path| path.as_str().into())
            .collect();
        if prefix.is_empty() || prefix == "/" {
            self.merge(probes)
        } else {
            self.nest(prefix, probes)
        }
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
        normalize_root(&mut document.paths)?;
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
        for (path, handlers) in self.extensions {
            let handlers = Arc::new(
                handlers
                    .into_iter()
                    .map(|(method, handler)| (method, handler.with_state::<()>(state.clone())))
                    .collect::<HashMap<_, _>>(),
            );
            let dispatch = move |request: Request| {
                let handler = handlers.get(request.method()).cloned();
                async move {
                    if let Some(handler) = handler {
                        let Ok(response) = handler.oneshot(request).await;
                        response
                    } else {
                        sanitized_internal_error()
                    }
                }
            };
            #[expect(
                clippy::disallowed_methods,
                reason = "extension methods share a fallback selected by their exact registered method"
            )]
            {
                self.routes = self.routes.route(&path, axum::routing::any(dispatch));
            }
        }
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
    extension_allow: HashMap<Box<str>, HeaderValue>,
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

fn documented_methods(item: &PathItem) -> impl Iterator<Item = &str> {
    fixed_path_operations!(item, as_ref)
        .into_iter()
        .filter_map(|(method, operation)| operation.map(|_| method))
        .chain(item.additional_operations.keys().map(String::as_str))
}

fn check_path_merge(
    path: &str,
    existing: &PathItem,
    incoming: &PathItem,
) -> Result<(), FinalizeError> {
    let mut incoming_methods = documented_methods(incoming).peekable();
    if incoming_methods.peek().is_none() && incoming.ref_path.is_none() {
        return Ok(());
    }
    let existing_fixed = fixed_path_operations!(existing, as_ref);
    if let Some(method) = incoming_methods.find(|method| {
        existing.additional_operations.contains_key(*method)
            || existing_fixed
                .iter()
                .any(|(name, op)| name == method && op.is_some())
    }) {
        return Err(FinalizeError::DuplicateOperation {
            path: path.to_owned(),
            method: method.to_owned(),
        });
    }
    let left = existing.parameters.as_deref().unwrap_or_default();
    let right = incoming.parameters.as_deref().unwrap_or_default();
    if existing.ref_path != incoming.ref_path {
        return Err(FinalizeError::ConflictingPathItem {
            path: path.to_owned(),
            field: "$ref",
        });
    }
    if left.len() != right.len() || !left.iter().all(|parameter| right.contains(parameter)) {
        return Err(FinalizeError::ConflictingPathItem {
            path: path.to_owned(),
            field: "parameters",
        });
    }
    if existing.servers != incoming.servers {
        return Err(FinalizeError::ConflictingPathItem {
            path: path.to_owned(),
            field: "servers",
        });
    }
    Ok(())
}

fn normalize_root(paths: &mut Paths) -> Result<(), FinalizeError> {
    if let Some(item) = paths.paths.remove("") {
        if let Some(root) = paths.paths.get_mut("/") {
            check_path_merge("/", root, &item)?;
            root.merge_operations(item);
        } else {
            paths.paths.insert("/".to_owned(), item);
        }
    }
    Ok(())
}

impl Policy {
    fn compile(
        document: &OpenApi,
        // template:begin http-idempotency:http-compile-mounts-parameter
        mounted: &MountedOperations,
        // template:end http-idempotency:http-compile-mounts-parameter
    ) -> Result<Self, FinalizeError> {
        let mut routes = HashMap::with_capacity(document.paths.paths.len());
        let mut extension_allow = HashMap::default();
        let mut first_protected = None;
        for (path, item) in &document.paths.paths {
            let path = if path.is_empty() {
                "/".to_owned()
            } else {
                path.clone()
            };
            let mut ordered_methods = BTreeMap::new();
            if item.query.is_some() || !item.additional_operations.is_empty() {
                let methods = documented_methods(item)
                    .chain((item.get.is_some() && item.head.is_none()).then_some("HEAD"))
                    .join(",");
                let allow = HeaderValue::from_str(&methods)
                    .map_err(|_| FinalizeError::InvalidRegistration)?;
                extension_allow.insert(path.as_str().into(), allow);
            }
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
            extension_allow,
            first_protected,
        })))
    }

    fn first_protected(&self) -> Option<(&str, &str)> {
        self.0
            .first_protected
            .as_ref()
            .map(|(path, method)| (&**path, &**method))
    }

    pub(crate) async fn unregistered(&self, path: Option<&str>, method: &Method) -> Response {
        if let Some(allow) = path.and_then(|path| self.0.extension_allow.get(path)) {
            let mut response = crate::harden::method_not_allowed().await;
            response.headers_mut().insert(ALLOW, allow.clone());
            return response;
        }
        tracing::error!(route = path, %method, failure = "policy_absent", "http_contract_wiring_failed");
        sanitized_internal_error()
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
        policy.unregistered(route, request.method()).await
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

    fn app(contract: HttpContract<()>) -> Application {
        public_app(contract).unwrap()
    }

    fn public_app(contract: HttpContract<()>) -> Result<Application, FinalizeError> {
        contract.build(
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
        let query = serde_json::from_value(serde_json::json!({
            "responses": {"200": {"description": "ok"}, "503": {
                    "description": "index rebuilding",
                    "content": {"text/plain": {"schema": {"type": "string"}}}
            }}
        }))
        .expect("valid OpenAPI fixture");
        let copy =
            serde_json::from_value(serde_json::json!({"responses":{"200":{"description":"ok"}}}))
                .unwrap();
        let contract = HttpContract::default()
            .extension(
                "/_test/method",
                Method::from_bytes(b"QUERY").unwrap(),
                query,
                || async { "query handler" },
            )
            .unwrap()
            .merge(
                HttpContract::default()
                    .extension(
                        "/_test/method",
                        Method::from_bytes(b"COPY").unwrap(),
                        copy,
                        || async { "copy handler" },
                    )
                    .unwrap(),
            )
            .unwrap()
            .merge(crate::http_route!(documented_get).unwrap())
            .unwrap();
        let contract = HttpContract::default().nest("/api", contract).unwrap();
        let published = contract.clone().into_openapi().unwrap();
        let published = serde_json::to_value(published).unwrap();
        let item = &published["paths"]["/api/_test/method"];
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
        let app = app(contract);

        for (method, body) in [
            ("QUERY", "query handler"),
            ("COPY", "copy handler"),
            ("GET", "get"),
            ("HEAD", ""),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/api/_test/method")
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
                body.as_bytes()
            );
        }
        let response = app
            .oneshot(
                Request::post("/api/_test/method")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let allow = response.headers()[ALLOW]
            .to_str()
            .unwrap()
            .split(',')
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            allow,
            ["COPY", "GET", "HEAD", "QUERY"].into_iter().collect()
        );
        assert_eq!(body_json(response).await["code"], "method_not_allowed");
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
            let contract = HttpContract::<()>::annotated(document, || async {}).unwrap();
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
        assert_eq!(
            HttpContract::<()>::default()
                .extension(
                    "/_test/method",
                    Method::POST,
                    Operation::default(),
                    || async {}
                )
                .unwrap_err(),
            FinalizeError::NativeMethod {
                method: "POST".into()
            }
        );
    }

    #[test]
    fn static_export_rejects_two_declarations_for_the_root_operation() {
        let operation = serde_json::json!({"get": {"responses": {"200": {"description": "ok"}}}});
        let document = serde_json::from_value(serde_json::json!({
            "openapi": "3.1.0", "info": {"title":"fixture","version":"1"},
            "paths": {"": operation, "/": operation}
        }))
        .unwrap();
        assert_eq!(
            HttpContract::<()>::annotated(document, || async {}).unwrap_err(),
            FinalizeError::InvalidRegistration
        );
    }

    #[test]
    fn application_defaults_cannot_be_mounted_as_a_feature() {
        let mut metadata = Api::openapi();
        metadata.security = Some(Vec::new());
        let root = HttpContract::<()>::application(metadata).unwrap();
        assert_eq!(
            HttpContract::default().merge(root.clone()).unwrap_err(),
            FinalizeError::ApplicationMetadata
        );
        assert_eq!(
            HttpContract::default().nest("/v1", root).unwrap_err(),
            FinalizeError::ApplicationMetadata
        );
        let schemes = serde_json::from_value(serde_json::json!({
            "securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer"}}
        }))
        .unwrap();
        assert_eq!(
            HttpContract::<()>::default()
                .with_components(schemes)
                .unwrap_err(),
            FinalizeError::ApplicationMetadata
        );
    }

    #[test]
    fn nesting_a_get_preserves_the_parent_head_policy() {
        #[utoipa::path(head, path = "/api/_test/method", security(("bearerAuth" = [])), responses((status = 204)))]
        async fn protected_head() -> StatusCode {
            StatusCode::NO_CONTENT
        }
        let document = serde_json::from_value(serde_json::json!({
            "openapi": "3.1.0", "info": {"title": "fixture", "version": "1"}, "paths": {},
            "components": {"securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}}}
        }))
        .unwrap();
        let parent = HttpContract::<()>::application(document)
            .unwrap()
            .merge(crate::http_route!(protected_head).unwrap())
            .unwrap();
        let child = crate::http_route!(documented_get).unwrap();
        let contract = parent.nest("/api", child).unwrap();
        let document = contract.into_openapi().unwrap();
        let path = &document.paths.paths["/api/_test/method"];
        assert!(path.get.is_some());
        assert!(path.head.as_ref().is_some_and(|operation| {
            operation
                .security
                .as_ref()
                .is_some_and(|rules| !rules.is_empty())
        }));
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
            let mut root: utoipa::openapi::OpenApi =
                serde_json::from_value(document.clone()).unwrap();
            let mut feature = Api::openapi();
            feature.paths = std::mem::take(&mut root.paths);
            let contract = HttpContract::application(root)
                .unwrap()
                .merge(HttpContract::annotated(feature, || async {}).unwrap())
                .unwrap();
            assert_eq!(
                public_app(contract.clone()).map(|_| ()),
                expected,
                "{document}"
            );
            let export = contract.into_openapi().map(|_| ());
            match &expected {
                Err(FinalizeError::ProtectedOperation { .. }) => assert_eq!(export, Ok(())),
                _ => assert_eq!(export, expected, "{document}"),
            }
        }
    }

    #[tokio::test]
    async fn implicit_head_inherits_get_but_native_method_fallback_remains() {
        let app = app(crate::http_route!(documented_get).unwrap());
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
        let app = app(HttpContract::default()
            .nest(prefix, crate::http_route!(documented_get).unwrap())
            .unwrap());
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
        let mut contract = crate::http_route!(documented_get).unwrap();
        let handler_1 = || async { "must not run" };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        {
            contract.routes = contract
                .routes
                .route("/undocumented", axum::routing::post(handler_1));
        }
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
