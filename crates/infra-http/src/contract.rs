use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::extract::{NestedPath, Request, State};
use axum::handler::Handler;
use axum::http::Method;
use axum::middleware;
use axum::response::Response;
use axum::routing::{MethodFilter, MethodRouter};
use foldhash::HashMap;
use itertools::Itertools as _;
use tower::ServiceExt as _;
use utoipa::openapi::path::{Operation, PathItem, Paths};
use utoipa::openapi::{Components, OpenApi};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use crate::HardenOptions;
use crate::probes::{ProbeRoute, ProbeRoutes};
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::sanitized_internal_error;

mod components;
mod document;
mod outcomes;
mod policy;

pub(crate) use components::ComponentKind;
use document::{StandardMethod, documented_methods, has_extension_operations};
use outcomes::ResponseFault;
// template:begin authn:http-contract-policy-reexports
pub(crate) use policy::{Access, contract_path};
// template:end authn:http-contract-policy-reexports
use policy::enforce_public;
pub(crate) use policy::{Policy, PolicyFault};

#[macro_export]
macro_rules! http_route {
    ($($handler:ident)::+) => {{
        $crate::HttpContract::from_annotation($crate::annotated_routes!($($handler)::+))
    }};
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(transparent)]
pub struct FinalizeError(pub(crate) FinalizeKind);

impl FinalizeError {
    pub(crate) const fn new(kind: FinalizeKind) -> Self {
        Self(kind)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathItemField {
    Ref,
    Parameters,
    Servers,
}

impl std::fmt::Display for PathItemField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ref => "$ref",
            Self::Parameters => "parameters",
            Self::Servers => "servers",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum FinalizeKind {
    #[error("application metadata must contain no registered paths")]
    ApplicationPaths,
    #[error("QUERY and additional operations require OpenAPI 3.2 application metadata")]
    OpenApi32Required,
    #[error("only the application root may supply global security, schemes, or metadata")]
    ApplicationMetadata,
    #[error("an annotated registration requires one concrete path with native HTTP operations")]
    InvalidRegistration,
    #[error("{method} must be registered as an annotated HTTP operation")]
    NativeMethod { method: String },
    // template:begin http-idempotency:http-store-finalization-error
    #[error("mounted HTTP idempotency operations require an active idempotency store")]
    MissingIdempotencyStore,
    #[error(transparent)]
    Idempotency(crate::idempotency::CompositionError),
    // template:end http-idempotency:http-store-finalization-error
    #[error(
        "HTTP contract finalization failed: components.{kind}.{name} conflicts with the shared HTTP contract"
    )]
    ConflictingComponent { kind: ComponentKind, name: String },
    #[error("HTTP contract composition defines {method} {path} more than once")]
    DuplicateOperation { path: String, method: String },
    #[error("HTTP contract composition cannot merge {path} with different path-level {field}")]
    ConflictingPathItem { path: String, field: PathItemField },
    #[error("HTTP contract finalization failed: {method} {path} response {status}: {fault}")]
    InvalidResponse {
        method: String,
        path: String,
        status: String,
        fault: ResponseFault,
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
        fault: PolicyFault,
    },
}

#[derive(Debug)]
pub enum Authentication {
    Public,
    // template:begin authn:http-assembly-verifier
    Bearer(infra_bearerauthn::Verifier),
    // template:end authn:http-assembly-verifier
}

#[derive(Clone)]
#[derive_where::derive_where(Default; S: Clone + Send + Sync + 'static)]
pub struct HttpContract<S> {
    routes: OpenApiRouter<S>,
    is_application_root: bool,
    extension_routers: HashMap<Method, Router<S>>,
    probe_paths: Vec<Box<str>>,
    // template:begin http-idempotency:http-mounted-operations-field
    idempotency: IdempotentOperations,
    // template:end http-idempotency:http-mounted-operations-field
}

#[derive(Clone)]
struct OuterExtensions(axum::http::Extensions);

impl OuterExtensions {
    fn stash(request: &mut Request) {
        let extensions = std::mem::take(request.extensions_mut());
        if let Some(nested) = extensions.get::<NestedPath>() {
            request.extensions_mut().insert(nested.clone());
        }
        request.extensions_mut().insert(Self(extensions));
    }

    fn restore(request: &mut Request) -> bool {
        let Some(Self(mut extensions)) = request.extensions_mut().remove() else {
            return false;
        };
        if let Some(nested) = request.extensions_mut().remove::<NestedPath>() {
            extensions.insert(nested);
        }
        *request.extensions_mut() = extensions;
        true
    }
}

async fn dispatch_extension(service: Router, mut request: Request) -> Response {
    OuterExtensions::stash(&mut request);
    let Ok(response) = service.oneshot(request).await;
    response
}

fn extension_router<S, H, T>(path: &str, handler: H) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    H: Handler<T, S>,
    T: 'static,
{
    let dispatch = move |State(state): State<S>, mut request: Request| {
        let handler = handler.clone();
        async move {
            if !OuterExtensions::restore(&mut request) {
                return sanitized_internal_error();
            }
            handler.call(request, state).await
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the dispatcher selects this handler by its exact registered extension method"
    )]
    Router::new().route(path, axum::routing::any(dispatch))
}

fn mount_extensions<S>(
    mut routes: Router<S>,
    extension_routers: HashMap<Method, Router<S>>,
    paths: &Paths,
    state: &S,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    if extension_routers.is_empty() {
        return routes;
    }
    let extension_routers = Arc::new(
        extension_routers
            .into_iter()
            .map(|(method, router)| (method, router.with_state::<()>(state.clone())))
            .collect::<HashMap<_, _>>(),
    );
    let dispatch = move |request: Request| {
        let selected = extension_routers.get(request.method()).cloned();
        async move {
            match selected {
                Some(service) => dispatch_extension(service, request).await,
                None => sanitized_internal_error(),
            }
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "extension methods share a fallback selected by their exact registered method"
    )]
    {
        for (path, item) in &paths.paths {
            if document::needs_extension_dispatch(item) {
                routes = routes.route(path, axum::routing::any(dispatch.clone()));
            }
        }
    }
    routes
}

// template:begin http-idempotency:http-mounted-operations
pub(crate) use components::{merge_map, same_definition};
pub(crate) use document::path_operations_mut;

pub(crate) fn resolve_schema<'a>(
    schema: &'a utoipa::openapi::RefOr<utoipa::openapi::Schema>,
    schemas: &'a std::collections::BTreeMap<
        String,
        utoipa::openapi::RefOr<utoipa::openapi::Schema>,
    >,
) -> Option<&'a utoipa::openapi::Schema> {
    document::resolve_when(
        schema,
        schemas,
        ComponentKind::Schemas,
        document::is_bare_reference,
    )
}

#[derive(Clone, Default)]
pub(crate) struct IdempotentOperations(Vec<MountedOperation>);

#[derive(Clone)]
struct MountedOperation {
    path: Box<str>,
    method: &'static str,
}

impl IdempotentOperations {
    pub(crate) fn contains(&self, path: &str, method: &str) -> bool {
        self.0
            .iter()
            .any(|operation| operation.path.as_ref() == path && operation.method == method)
    }

    fn register(&mut self, path: &str, method: &'static str) {
        self.0.push(MountedOperation {
            path: path.into(),
            method,
        });
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn append(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    fn nest(&mut self, prefix: &str) {
        for operation in &mut self.0 {
            operation.path = nested_path(prefix, &operation.path);
        }
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
            return Err(FinalizeError::new(FinalizeKind::ApplicationPaths));
        }
        Ok(Self {
            routes: OpenApiRouter::with_openapi(metadata),
            is_application_root: true,
            ..Self::default()
        })
    }

    pub fn with_components(mut self, incoming: Components) -> Result<Self, FinalizeError> {
        if !incoming.security_schemes.is_empty() {
            return Err(FinalizeError::new(FinalizeKind::ApplicationMetadata));
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
    pub fn from_annotation(
        (schemas, paths, router): UtoipaMethodRouter<S>,
    ) -> Result<Self, FinalizeError> {
        validate_single_native_path(&paths)?;
        let mut components = Components::default();
        components::merge_map(ComponentKind::Schemas, &mut components.schemas, schemas)?;
        Self::from_route(
            utoipa::openapi::OpenApiBuilder::new()
                .components(Some(components))
                .build(),
            (Vec::new(), paths, router),
        )
    }

    #[cfg(test)]
    fn annotated<H, T>(mut document: OpenApi, handler: H) -> Result<Self, FinalizeError>
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let paths = std::mem::take(&mut document.paths);
        let filter = validate_single_native_path(&paths)?;
        Self::from_route(
            document,
            (Vec::new(), paths, axum::routing::on(filter, handler)),
        )
    }

    fn from_route(metadata: OpenApi, route: UtoipaMethodRouter<S>) -> Result<Self, FinalizeError> {
        if metadata.security.is_some()
            || !metadata.paths.paths.is_empty()
            || metadata.servers.is_some()
            || metadata.tags.is_some()
            || metadata.webhooks.is_some()
            || metadata.external_docs.is_some()
            || metadata
                .extensions
                .as_ref()
                .is_some_and(|extensions| !extensions.is_empty())
            || metadata.json_schema_dialect.is_some()
            || !metadata.schema.is_empty()
            || metadata.self_uri.is_some()
        {
            return Err(FinalizeError::new(FinalizeKind::ApplicationMetadata));
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
        if method != Method::CONNECT && MethodFilter::try_from(method.clone()).is_ok() {
            return Err(FinalizeError::new(FinalizeKind::NativeMethod {
                method: method.to_string(),
            }));
        }
        let path = if path.is_empty() { "/" } else { path };
        if !path.starts_with('/') {
            return Err(FinalizeError::new(FinalizeKind::InvalidRegistration));
        }
        let mut item = PathItem::default();
        if method.as_str() == StandardMethod::Query.as_str() {
            item.query = Some(operation);
        } else {
            item.additional_operations
                .insert(method.to_string(), operation);
        }
        let mut paths = Paths::new();
        paths.paths.insert(path.to_owned(), item);
        if method == Method::CONNECT {
            let route = (
                Vec::new(),
                paths,
                axum::routing::on(MethodFilter::CONNECT, handler),
            );
            return self.merge(Self::from_route(OpenApi::default(), route)?);
        }
        let mut feature =
            Self::from_route(OpenApi::default(), (Vec::new(), paths, MethodRouter::new()))?;
        feature
            .extension_routers
            .insert(method, extension_router(path, handler));
        self.merge(feature)
    }

    pub fn with_state<S2: Clone + Send + Sync + 'static>(self, state: S) -> HttpContract<S2> {
        HttpContract {
            routes: self.routes.with_state(state.clone()),
            extension_routers: self
                .extension_routers
                .into_iter()
                .map(|(method, router)| (method, router.with_state(state.clone())))
                .collect(),
            probe_paths: self.probe_paths,
            is_application_root: self.is_application_root,
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
        let mut contract = Self::from_route(metadata, route)?;
        for (path, item) in &contract.routes.get_openapi().paths.paths {
            for (method, _) in document::standard_operations(item) {
                contract.idempotency.register(path, method);
            }
        }
        Ok(contract)
    }

    #[must_use]
    pub fn idempotent_operations(&self) -> usize {
        self.idempotency.len()
    }
    // template:end http-idempotency:http-typed-registration

    pub fn merge(mut self, mut other: Self) -> Result<Self, FinalizeError> {
        if other.is_application_root {
            return Err(FinalizeError::new(FinalizeKind::ApplicationMetadata));
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
        for (method, router) in other.extension_routers {
            let existing = self.extension_routers.entry(method).or_default();
            *existing = std::mem::take(existing).merge(router);
        }
        self.probe_paths.extend(other.probe_paths);
        // template:begin http-idempotency:http-merge-mounted-operations
        self.idempotency.append(other.idempotency);
        // template:end http-idempotency:http-merge-mounted-operations
        Ok(self)
    }

    pub fn nest(self, prefix: &str, mut other: Self) -> Result<Self, FinalizeError> {
        if other.is_application_root {
            return Err(FinalizeError::new(FinalizeKind::ApplicationMetadata));
        }
        normalize_root(&mut other.routes.get_openapi_mut().paths)?;
        let components = other.routes.get_openapi_mut().components.take();
        other.routes = OpenApiRouter::default().nest(prefix, other.routes);
        other.routes.get_openapi_mut().components = components;
        for router in other.extension_routers.values_mut() {
            *router = Router::new().nest(prefix, std::mem::take(router));
        }
        for path in &mut other.probe_paths {
            *path = nested_path(prefix, path);
        }
        // template:begin http-idempotency:http-nest-mounted-operations
        other.idempotency.nest(prefix);
        // template:end http-idempotency:http-nest-mounted-operations
        self.merge(other)
    }

    pub fn with_probes(self, prefix: &str) -> Result<Self, FinalizeError>
    where
        health::ReadinessReader: axum::extract::FromRef<S>,
    {
        let mut probe_contract = crate::probes::router()?;
        probe_contract.probe_paths = probe_contract
            .routes
            .get_openapi()
            .paths
            .paths
            .keys()
            .map(|path| path.as_str().into())
            .collect();
        if prefix.is_empty() || prefix == "/" {
            self.merge(probe_contract)
        } else {
            self.nest(prefix, probe_contract)
        }
    }

    fn probe_routes(&self) -> ProbeRoutes {
        let paths = &self.routes.get_openapi().paths.paths;
        ProbeRoutes::new(
            self.probe_paths
                .iter()
                .map(|path| {
                    let implicit_head = paths
                        .get(path.as_ref())
                        .is_some_and(|item| item.head.is_none());
                    ProbeRoute {
                        path: path.clone(),
                        implicit_head,
                    }
                })
                .collect(),
        )
    }

    pub fn into_openapi(mut self) -> Result<OpenApi, FinalizeError> {
        self.finalize(&self.probe_routes())?;
        Ok(self.routes.into_openapi())
    }

    fn finalize(&mut self, probe_routes: &ProbeRoutes) -> Result<Policy, FinalizeError> {
        let document = self.routes.get_openapi_mut();
        normalize_root(&mut document.paths)?;
        if document.paths.paths.values().any(has_extension_operations) {
            use utoipa::openapi::OpenApiVersion;
            if self.is_application_root && document.openapi != OpenApiVersion::Version32 {
                return Err(FinalizeError::new(FinalizeKind::OpenApi32Required));
            }
            document.openapi = OpenApiVersion::Version32;
        }
        let policy = Policy::compile(
            document,
            // template:begin http-idempotency:http-policy-compile-mounts
            &self.idempotency,
            // template:end http-idempotency:http-policy-compile-mounts
        )?;
        outcomes::compose(
            document,
            probe_routes,
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
        let probe_routes = self.probe_routes();
        let policy = self.finalize(&probe_routes)?;
        let (routes, document) = self.routes.split_for_parts();
        let routes = mount_extensions(routes, self.extension_routers, &document.paths, &state);
        let routes = authentication.enforce(routes, policy)?;
        // template:begin http-idempotency:http-build-bind-store
        let routes = if self.idempotency.is_empty() {
            routes
        } else {
            routes.layer(axum::Extension(store.ok_or_else(|| {
                FinalizeError::new(FinalizeKind::MissingIdempotencyStore)
            })?))
        };
        // template:end http-idempotency:http-build-bind-store
        Ok(Application(crate::harden::assemble(
            routes.with_state(state),
            options,
            probe_routes,
        )))
    }
}

#[derive(Clone, Debug)]
pub struct Application(Router);

impl Application {
    #[must_use]
    pub fn with_extension<T: Clone + Send + Sync + 'static>(self, value: T) -> Self {
        Self(self.0.layer(axum::Extension(value)))
    }

    pub fn serve(
        self,
        listener: infra_http_transport::BoundListener,
        options: infra_http_transport::Http1Options,
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

impl Authentication {
    fn enforce<S>(self, routes: Router<S>, policy: Policy) -> Result<Router<S>, FinalizeError>
    where
        S: Clone + Send + Sync + 'static,
    {
        match self {
            Self::Public => {
                if let Some((path, method)) = policy.first_protected() {
                    return Err(FinalizeError::new(FinalizeKind::ProtectedOperation {
                        path: path.to_owned(),
                        method: method.to_owned(),
                    }));
                }
                Ok(layer_routes(routes, |router| {
                    router.route_layer(middleware::from_fn_with_state(policy, enforce_public))
                }))
            }
            // template:begin authn:http-assembly-finalize
            Self::Bearer(auth) => Ok(crate::authn::finalize(routes, policy, auth)),
            // template:end authn:http-assembly-finalize
        }
    }
}

pub(crate) fn layer_routes<S>(
    router: Router<S>,
    layer: impl FnOnce(Router<S>) -> Router<S>,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    if router.has_routes() {
        layer(router)
    } else {
        router
    }
}

fn validate_single_native_path(paths: &Paths) -> Result<MethodFilter, FinalizeError> {
    let invalid = || FinalizeError::new(FinalizeKind::InvalidRegistration);
    let item = paths.paths.values().exactly_one().map_err(|_| invalid())?;
    if item.ref_path.is_some() || !item.additional_operations.is_empty() {
        return Err(invalid());
    }
    let method = documented_methods(item)
        .exactly_one()
        .map_err(|_| invalid())?;
    Method::from_bytes(method.as_bytes())
        .ok()
        .and_then(|method| MethodFilter::try_from(method).ok())
        .ok_or_else(invalid)
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
    if let Some(method) = incoming_methods.find(|method| {
        existing.additional_operations.contains_key(*method)
            || StandardMethod::parse(method)
                .and_then(|standard| standard.operation(existing))
                .is_some()
    }) {
        return Err(FinalizeError::new(FinalizeKind::DuplicateOperation {
            path: path.to_owned(),
            method: method.to_owned(),
        }));
    }
    let conflict = |field| {
        FinalizeError::new(FinalizeKind::ConflictingPathItem {
            path: path.to_owned(),
            field,
        })
    };
    if existing.ref_path != incoming.ref_path {
        return Err(conflict(PathItemField::Ref));
    }
    let existing_parameters = existing.parameters.as_deref().unwrap_or_default();
    let incoming_parameters = incoming.parameters.as_deref().unwrap_or_default();
    if existing_parameters.len() != incoming_parameters.len()
        || !existing_parameters
            .iter()
            .all(|parameter| incoming_parameters.contains(parameter))
    {
        return Err(conflict(PathItemField::Parameters));
    }
    if existing.servers != incoming.servers {
        return Err(conflict(PathItemField::Servers));
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
                trust_trace_context: false,
                draining: None,
            },
        )
    }

    fn mixed_method_contract() -> HttpContract<()> {
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
            .unwrap()
            .extension(
                "/_test/method",
                Method::CONNECT,
                serde_json::from_value(
                    serde_json::json!({"responses":{"200":{"description":"connected"}}}),
                )
                .unwrap(),
                || async { StatusCode::OK },
            )
            .unwrap();
        HttpContract::default().nest("/api", contract).unwrap()
    }

    #[tokio::test]
    async fn public_query_and_additional_method_reach_the_registered_service() {
        let contract = mixed_method_contract();
        let published = contract.clone().into_openapi().unwrap();
        let published = serde_json::to_value(published).unwrap();
        assert_eq!(published["openapi"], "3.2.0");
        assert_eq!(
            HttpContract::application(utoipa::openapi::OpenApi::default())
                .unwrap()
                .merge(contract.clone())
                .unwrap()
                .into_openapi()
                .err(),
            Some(FinalizeError::new(FinalizeKind::OpenApi32Required))
        );
        let item = &published["paths"]["/api/_test/method"];
        assert_eq!(
            item["additionalOperations"]["CONNECT"]["responses"]["200"]["description"],
            "connected"
        );
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
            ("CONNECT", ""),
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
            ["CONNECT", "COPY", "GET", "HEAD", "QUERY"]
                .into_iter()
                .collect()
        );
        assert_eq!(body_json(response).await["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn extension_methods_retain_their_own_nested_request_context() {
        async fn context(
            uri: axum::http::Uri,
            original: axum::extract::OriginalUri,
            nested: NestedPath,
            matched: axum::extract::MatchedPath,
            crate::extract::Path((tenant, id)): crate::extract::Path<(String, String)>,
            axum::Extension(marker): axum::Extension<u32>,
        ) -> String {
            format!(
                "{uri} {} {} {} {tenant} {id} {marker}",
                original.0,
                nested.as_str(),
                matched.as_str()
            )
        }
        let operation: Operation = serde_json::from_value(
            serde_json::json!({"responses":{"200":{"description":"context"}}}),
        )
        .unwrap();
        let query = HttpContract::default()
            .extension(
                "/books/{id}",
                Method::from_bytes(b"QUERY").unwrap(),
                operation.clone(),
                context,
            )
            .unwrap();
        let copy = HttpContract::default()
            .extension(
                "/v1/books/{id}",
                Method::from_bytes(b"COPY").unwrap(),
                operation,
                context,
            )
            .unwrap();
        let service = app(HttpContract::default()
            .nest("/tenants/{tenant}", copy.nest("/v1", query).unwrap())
            .unwrap());
        for (method, local, prefix) in [
            ("QUERY", "/books/two?x=1", "/tenants/{tenant}/v1"),
            ("COPY", "/v1/books/two?x=1", "/tenants/{tenant}"),
        ] {
            let response = service
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/tenants/one/v1/books/two?x=1")
                        .extension(17u32)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body.as_ref(), format!("{local} /tenants/one/v1/books/two?x=1 {prefix} /tenants/{{tenant}}/v1/books/{{id}} one two 17").as_bytes());
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
            let contract = HttpContract::<()>::annotated(document, || async {}).unwrap();
            assert_eq!(
                contract.into_openapi().map(|_| ()),
                Err(FinalizeError::new(FinalizeKind::InvalidPolicy {
                    method: "POST".to_owned(),
                    path: "/ordinary".to_owned(),
                    fault: PolicyFault::IdempotencyRequiresRegistration,
                }))
            );
        }
    }
    // template:end http-idempotency:http-unregistered-contract-test

    #[test]
    fn json_body_outcome_respects_declared_media_types_and_responses() {
        let explicit = serde_json::json!({
            "description": "unsupported widget version",
            "content": {"text/plain": {"schema": {"type": "string"}}},
            "headers": {"Accept": {"schema": {"type": "string"}}}
        });
        let local = serde_json::json!({"$ref": "#/components/responses/UnsupportedWidgetVersion"});
        let opaque = serde_json::json!({"$ref": "https://api.example/problems.yaml#/Unsupported"});
        let mut opaque_content = explicit.clone();
        opaque_content["content"]["application/problem+json"] = serde_json::json!({
            "$ref": "https://api.example/problems.yaml#/components/mediaTypes/Unsupported"
        });
        let json_body = serde_json::json!({"$ref": "#/components/requestBodies/Widget"});
        let mixed_body = serde_json::json!({
            "content": {"application/json": {}, "application/octet-stream": {}}
        });
        let application = utoipa::openapi::OpenApiBuilder::new()
            .openapi(utoipa::openapi::OpenApiVersion::Version32)
            .build();
        for (body, response, adds_problem) in [
            (&json_body, &explicit, true),
            (&json_body, &local, true),
            (&json_body, &opaque_content, false),
            (&json_body, &opaque, false),
            (&mixed_body, &explicit, false),
        ] {
            let document = serde_json::from_value(serde_json::json!({
                "openapi": "3.2.0",
                "info": {"title": "fixture", "version": "1"},
                "components": {
                    "requestBodies": {"Widget": {
                        "content": {"application/vnd.widget+json": {}}
                    }},
                    "responses": {"UnsupportedWidgetVersion": explicit},
                },
                "paths": {"/widgets": {"post": {
                    "requestBody": body,
                    "responses": {"204": {"description": "updated"}, "415": response}
                }}}
            }))
            .unwrap();
            let feature = HttpContract::<()>::annotated(document, || async {}).unwrap();
            let contract = HttpContract::application(application.clone())
                .unwrap()
                .merge(feature)
                .unwrap();
            let published = serde_json::to_value(contract.into_openapi().unwrap()).unwrap();
            assert_eq!(published["openapi"], "3.2.0");
            let media_type_response = &published["paths"]["/widgets"]["post"]["responses"]["415"];
            if adds_problem {
                assert_eq!(
                    media_type_response["content"]["application/problem+json"]["schema"]["$ref"],
                    "#/components/schemas/Problem"
                );
                assert_eq!(
                    media_type_response["content"]["text/plain"],
                    explicit["content"]["text/plain"]
                );
                assert_eq!(
                    media_type_response["headers"]["Accept"],
                    explicit["headers"]["Accept"]
                );
                assert!(
                    media_type_response["description"]
                        .as_str()
                        .unwrap()
                        .contains("unsupported widget version")
                );
            } else {
                assert_eq!(media_type_response, response);
            }
        }
    }

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
            FinalizeError::new(FinalizeKind::NativeMethod {
                method: "POST".into()
            })
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
            FinalizeError::new(FinalizeKind::InvalidRegistration)
        );
    }

    #[test]
    fn application_defaults_cannot_be_mounted_as_a_feature() {
        let mut metadata = Api::openapi();
        metadata.security = Some(Vec::new());
        let root = HttpContract::<()>::application(metadata).unwrap();
        assert_eq!(
            HttpContract::default().merge(root.clone()).unwrap_err(),
            FinalizeError::new(FinalizeKind::ApplicationMetadata)
        );
        assert_eq!(
            HttpContract::default().nest("/v1", root).unwrap_err(),
            FinalizeError::new(FinalizeKind::ApplicationMetadata)
        );
        let schemes = serde_json::from_value(serde_json::json!({
            "securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer"}}
        }))
        .unwrap();
        assert_eq!(
            HttpContract::<()>::default()
                .with_components(schemes)
                .unwrap_err(),
            FinalizeError::new(FinalizeKind::ApplicationMetadata)
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
        FinalizeError::new(FinalizeKind::ProtectedOperation {
            path: "/_test/policy".to_owned(),
            method: "GET".to_owned(),
        })
    }

    fn invalid(fault: PolicyFault) -> FinalizeError {
        FinalizeError::new(FinalizeKind::InvalidPolicy {
            path: "/_test/policy".to_owned(),
            method: "GET".to_owned(),
            fault,
        })
    }

    #[test]
    fn delegation_requires_a_boolean_opt_in_on_a_protected_operation() {
        for (value, fault) in [
            (serde_json::json!(true), PolicyFault::PublicDelegation),
            (serde_json::json!("true"), PolicyFault::InvalidDelegation),
        ] {
            let document = serde_json::from_value(serde_json::json!({
                "openapi": "3.1.0", "info": {"title":"fixture","version":"1"},
                "paths": {"/_test/policy": {"get": {
                    "x-bearer-delegation": value, "responses": {"200": {"description": "ok"}}
                }}}
            }))
            .unwrap();
            assert_eq!(
                public_app(HttpContract::annotated(document, || async {}).unwrap()).unwrap_err(),
                invalid(fault)
            );
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
                Err(invalid(PolicyFault::UnsupportedRequirement)),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": ["write"]}])),
                None,
                Err(protected()),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": ["read", "write"]}, {"bearerAuth": []}])),
                None,
                Err(protected()),
            ),
            (
                None,
                Some(
                    serde_json::json!([{"bearerAuth": ["read"]}, {"bearerAuth": ["admin", "read write"]}]),
                ),
                None,
                Err(invalid(PolicyFault::InvalidScopeToken)),
            ),
            (
                Some(serde_json::json!([{"bearerAuth": [""]}])),
                None,
                None,
                Err(invalid(PolicyFault::InvalidScopeToken)),
            ),
            (
                None,
                Some(serde_json::json!([{"unknown": []}])),
                None,
                Err(invalid(PolicyFault::UnsupportedRequirement)),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": []}, {}])),
                None,
                Err(invalid(PolicyFault::UnsupportedRequirement)),
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
                Err(FinalizeError(FinalizeKind::ProtectedOperation { .. })) => {
                    assert_eq!(export, Ok(()));
                }
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
