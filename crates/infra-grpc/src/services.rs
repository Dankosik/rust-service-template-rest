use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::{Arc, OnceLock};

use foldhash::HashMap;
use prost::Message as _;
use tonic::server::NamedService as _;
use tonic::service::LayerExt as _;
use tonic_reflection::pb::{v1, v1alpha};
use tower::util::{BoxCloneSyncService, BoxCloneSyncServiceLayer, MapRequestLayer};
use tower::{Layer, Service};

use crate::Error;
use crate::error::ErrorKind;
use crate::health::HealthServer;
use crate::observe::MethodMetrics;
use crate::protocol::method_path;
use crate::router::{Application, Authentication, ServerOptions};

mod reflection;

const REFLECTION_SERVICE: &str = v1::server_reflection_server::SERVICE_NAME;
const REFLECTION_DESCRIPTORS: [&[u8]; 3] = [
    v1::FILE_DESCRIPTOR_SET,
    v1alpha::FILE_DESCRIPTOR_SET,
    tonic_health::pb::FILE_DESCRIPTOR_SET,
];

pub(crate) const REFLECTION_PATHS: [&str; 2] = [
    "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CallType {
    Unary,
    ClientStream,
    ServerStream,
    BidiStream,
}

impl CallType {
    pub(crate) const fn new(client_streaming: bool, server_streaming: bool) -> Self {
        match (client_streaming, server_streaming) {
            (false, false) => Self::Unary,
            (true, false) => Self::ClientStream,
            (false, true) => Self::ServerStream,
            (true, true) => Self::BidiStream,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MethodKind {
    #[default]
    Unknown,
    Registered {
        call_type: CallType,
        end_on_drain: bool,
    },
}

impl MethodKind {
    pub(crate) const fn call_type(self) -> Option<CallType> {
        match self {
            Self::Unknown => None,
            Self::Registered { call_type, .. } => Some(call_type),
        }
    }

    pub(crate) const fn is_registered(self) -> bool {
        matches!(self, Self::Registered { .. })
    }

    pub(crate) const fn streams_response(self) -> bool {
        matches!(
            self,
            Self::Registered {
                call_type: CallType::ServerStream | CallType::BidiStream,
                ..
            }
        )
    }

    pub(crate) const fn end_on_drain(self) -> bool {
        matches!(
            self,
            Self::Registered {
                end_on_drain: true,
                ..
            }
        )
    }
}

#[derive(Default)]
pub(crate) enum MethodPolicy {
    #[default]
    None,
    // template:begin authn:grpc-method-policy-scopes
    Scopes(crate::authn::ScopeRequirement),
    // template:end authn:grpc-method-policy-scopes
    // template:begin service-auth:grpc-method-policy-callers
    Callers(foldhash::HashSet<infra_serviceauth::ServiceName>),
    // template:end service-auth:grpc-method-policy-callers
}

#[derive(Default)]
pub(crate) struct Method {
    pub(crate) kind: MethodKind,
    #[allow(
        dead_code,
        reason = "auth profiles read per-method policies; no-auth profiles keep the uniform None policy"
    )]
    pub(crate) policy: MethodPolicy,
    pub(crate) metrics: OnceLock<Arc<MethodMetrics>>,
}

impl Method {
    const fn new(kind: MethodKind, policy: MethodPolicy) -> Self {
        Self {
            kind,
            policy,
            metrics: OnceLock::new(),
        }
    }

    // template:begin authn:grpc-method-scope-requirement
    pub(crate) fn scope_requirement(&self) -> Option<&crate::authn::ScopeRequirement> {
        if let MethodPolicy::Scopes(requirement) = &self.policy {
            Some(requirement)
        } else {
            None
        }
    }
    // template:end authn:grpc-method-scope-requirement

    // template:begin service-auth:grpc-method-allowed-callers
    pub(crate) fn allowed_callers(
        &self,
    ) -> Option<&foldhash::HashSet<infra_serviceauth::ServiceName>> {
        if let MethodPolicy::Callers(callers) = &self.policy {
            Some(callers)
        } else {
            None
        }
    }
    // template:end service-auth:grpc-method-allowed-callers
}

pub(crate) struct Methods {
    registered: HashMap<Box<str>, Method>,
    unknown: Method,
}

impl Methods {
    pub(crate) fn new(
        paths: impl IntoIterator<Item = (Box<str>, CallType, bool, MethodPolicy)>,
    ) -> Self {
        Self {
            registered: paths
                .into_iter()
                .map(|(path, call_type, end_on_drain, policy)| {
                    (
                        path,
                        Method::new(
                            MethodKind::Registered {
                                call_type,
                                end_on_drain,
                            },
                            policy,
                        ),
                    )
                })
                .collect(),
            unknown: Method::default(),
        }
    }

    pub(crate) fn resolve(&self, path: &str) -> &Method {
        self.registered.get(path).unwrap_or(&self.unknown)
    }
}

pub type ListenerService = BoxCloneSyncService<
    http::Request<tonic::body::Body>,
    http::Response<tonic::body::Body>,
    Infallible,
>;

pub(crate) type ListenerLayer = BoxCloneSyncServiceLayer<
    ListenerService,
    http::Request<tonic::body::Body>,
    http::Response<tonic::body::Body>,
    Infallible,
>;

pub(crate) struct BuiltServices {
    pub(crate) routes: tonic::service::Routes,
    pub(crate) listener_layers: Vec<ListenerLayer>,
    pub(crate) names: BTreeSet<&'static str>,
    pub(crate) methods: Arc<Methods>,
    pub(crate) authentication: Authentication,
}

#[derive(Debug, Default)]
pub struct Services {
    listener_layers: Vec<ListenerLayer>,
    routes: tonic::service::RoutesBuilder,
    names: BTreeSet<&'static str>,
    described: HashMap<String, BTreeMap<String, CallType>>,
    descriptor_sets: Vec<&'static [u8]>,
    reflection: bool,
    // template:begin authn:grpc-services-scopes-field
    scopes: HashMap<String, crate::authn::ScopeRequirement>,
    // template:end authn:grpc-services-scopes-field
    // template:begin service-auth:grpc-services-callers-field
    caller_rules: HashMap<String, foldhash::HashSet<infra_serviceauth::ServiceName>>,
    // template:end service-auth:grpc-services-callers-field
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "optional auth profiles expose policy registration methods on this handle"
)]
pub struct ServiceRegistration<'a> {
    services: &'a mut Services,
    service_name: &'static str,
}

impl ServiceRegistration<'_> {
    // template:begin authn:grpc-services-require-scopes
    pub fn require_scopes(&mut self, method: &str, scopes: &[&str]) -> Result<&mut Self, Error> {
        let path = self.services.resolve_service_method(
            self.service_name,
            method,
            crate::error::PolicyKind::ScopeRequirement,
        )?;
        self.services
            .register_scopes_for_paths(&[path.as_str()], scopes, false)?;
        Ok(self)
    }

    pub fn require_scopes_for_delegation(
        &mut self,
        method: &str,
        scopes: &[&str],
    ) -> Result<&mut Self, Error> {
        let path = self.services.resolve_service_method(
            self.service_name,
            method,
            crate::error::PolicyKind::ScopeRequirement,
        )?;
        self.services
            .register_scopes_for_paths(&[path.as_str()], scopes, true)?;
        Ok(self)
    }
    // template:end authn:grpc-services-require-scopes

    // template:begin service-auth:grpc-services-allow-callers
    pub fn allow_callers(&mut self, method: &str, callers: &[&str]) -> Result<&mut Self, Error> {
        let path = self.services.resolve_service_method(
            self.service_name,
            method,
            crate::error::PolicyKind::CallerRule,
        )?;
        self.services
            .register_caller_rules_for_paths(&[path.as_str()], callers)?;
        Ok(self)
    }
    // template:end service-auth:grpc-services-allow-callers
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "optional auth profiles expose policy registration methods on this handle"
)]
pub struct ReflectionRegistration<'a> {
    services: &'a mut Services,
}

impl ReflectionRegistration<'_> {
    // template:begin authn:grpc-reflection-require-scopes
    pub fn require_scopes(&mut self, scopes: &[&str]) -> Result<&mut Self, Error> {
        self.services
            .register_scopes_for_paths(&REFLECTION_PATHS, scopes, false)?;
        Ok(self)
    }

    pub fn require_scopes_for_delegation(&mut self, scopes: &[&str]) -> Result<&mut Self, Error> {
        self.services
            .register_scopes_for_paths(&REFLECTION_PATHS, scopes, true)?;
        Ok(self)
    }
    // template:end authn:grpc-reflection-require-scopes

    // template:begin service-auth:grpc-reflection-allow-callers
    pub fn allow_callers(&mut self, callers: &[&str]) -> Result<&mut Self, Error> {
        self.services
            .register_caller_rules_for_paths(&REFLECTION_PATHS, callers)?;
        Ok(self)
    }
    // template:end service-auth:grpc-reflection-allow-callers
}

impl Services {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_listener_layer<L>(&mut self, layer: L) -> &mut Self
    where
        L: Layer<ListenerService> + Send + Sync + 'static,
        L::Service: Service<
                http::Request<tonic::body::Body>,
                Response = http::Response<tonic::body::Body>,
                Error = Infallible,
            > + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<http::Request<tonic::body::Body>>>::Future: Send + 'static,
    {
        self.listener_layers.push(ListenerLayer::new(layer));
        self
    }

    pub fn describe(&mut self, encoded_file_descriptor_set: &'static [u8]) -> Result<(), Error> {
        self.index_descriptor_services(encoded_file_descriptor_set)?;
        self.descriptor_sets.push(encoded_file_descriptor_set);
        Ok(())
    }

    pub(crate) fn index_descriptor_services(
        &mut self,
        encoded_file_descriptor_set: &[u8],
    ) -> Result<(), Error> {
        let set = prost_types::FileDescriptorSet::decode(encoded_file_descriptor_set)
            .map_err(|source| Error::new(ErrorKind::DecodeDescriptorSet(source)))?;
        let mut pending_descriptions: HashMap<String, BTreeMap<String, CallType>> =
            HashMap::default();
        for file in set.file {
            let package = file.package.as_deref().unwrap_or_default();
            for service in file.service {
                let name = service.name.unwrap_or_default();
                let name = if package.is_empty() {
                    name
                } else {
                    format!("{package}.{name}")
                };
                let methods: BTreeMap<_, _> = service
                    .method
                    .into_iter()
                    .map(|method| {
                        let call_type =
                            CallType::new(method.client_streaming(), method.server_streaming());
                        (method.name.unwrap_or_default(), call_type)
                    })
                    .collect();
                match pending_descriptions
                    .get(&name)
                    .or_else(|| self.described.get(&name))
                {
                    Some(previous) if previous != &methods => {
                        return Err(Error::new(ErrorKind::ConflictingServiceDescription(name)));
                    }
                    Some(_) => {}
                    None => {
                        pending_descriptions.insert(name, methods);
                    }
                }
            }
        }
        self.described.extend(pending_descriptions);
        Ok(())
    }

    pub fn add<S>(&mut self, service: S) -> Result<ServiceRegistration<'_>, Error>
    where
        S: tower::Service<http::Request<tonic::body::Body>, Error = Infallible>
            + tonic::server::NamedService
            + Clone
            + Send
            + Sync
            + 'static,
        S::Response: axum::response::IntoResponse,
        S::Future: Send + 'static,
    {
        if S::NAME == HealthServer::NAME {
            return Err(Error::new(ErrorKind::DuplicateService(S::NAME)));
        }
        if !self.described.contains_key(S::NAME) {
            return Err(Error::new(ErrorKind::UndescribedService(S::NAME)));
        }
        if !self.names.insert(S::NAME) {
            return Err(Error::new(ErrorKind::DuplicateService(S::NAME)));
        }
        self.routes.add_service(service);
        Ok(ServiceRegistration {
            services: self,
            service_name: S::NAME,
        })
    }

    pub fn add_reflection(&mut self) -> Result<ReflectionRegistration<'_>, Error> {
        if std::mem::replace(&mut self.reflection, true) {
            return Err(Error::new(ErrorKind::DuplicateService(REFLECTION_SERVICE)));
        }
        Ok(ReflectionRegistration { services: self })
    }

    pub fn build(
        self,
        readiness: ::health::ReadinessReader,
        authentication: Authentication,
        options: ServerOptions,
    ) -> Result<Application, Error> {
        crate::router::build(self, readiness, authentication, options)
    }

    pub(crate) fn into_built(
        mut self,
        authentication: Authentication,
    ) -> Result<BuiltServices, Error> {
        if self.reflection {
            self.attach_reflection()?;
        }
        self.index_descriptor_services(tonic_health::pb::FILE_DESCRIPTOR_SET)?;
        // template:begin service-auth:grpc-router-caller-rules
        if matches!(authentication, Authentication::ServiceTokens(_)) {
            self.validate_caller_rules()?;
        } else if !self.caller_rules.is_empty() {
            return Err(Error::new(ErrorKind::CallerRulesRequireServiceTokens));
        }
        // template:end service-auth:grpc-router-caller-rules
        // template:begin authn:grpc-router-authenticate
        if matches!(authentication, Authentication::None) {
            return Err(Error::unauthenticated_business_calls());
        }
        if matches!(authentication, Authentication::Bearer(_)) {
            self.prepare_scope_requirements()?;
        } else if self
            .scopes
            .values()
            .any(|rule| rule.delegate || !rule.scopes.is_empty())
        {
            return Err(Error::new(ErrorKind::ScopesWithServiceTokens));
        }
        // template:end authn:grpc-router-authenticate
        let Self {
            listener_layers,
            routes,
            names,
            described,
            reflection,
            // template:begin authn:grpc-router-bearer-destructure
            mut scopes,
            // template:end authn:grpc-router-bearer-destructure
            // template:begin service-auth:grpc-router-caller-destructure
            mut caller_rules,
            // template:end service-auth:grpc-router-caller-destructure
            ..
        } = self;
        let business =
            described_method_paths(&described, names.iter().copied()).map(|(path, kind)| {
                let end_on_drain = reflection && REFLECTION_PATHS.contains(&path.as_str());
                let policy = match &authentication {
                    Authentication::None => MethodPolicy::None,
                    // template:begin authn:grpc-method-policy-bearer
                    Authentication::Bearer(_) => scopes
                        .remove(&path)
                        .map_or(MethodPolicy::None, MethodPolicy::Scopes),
                    // template:end authn:grpc-method-policy-bearer
                    // template:begin service-auth:grpc-method-policy-service-tokens
                    Authentication::ServiceTokens(_) => caller_rules
                        .remove(&path)
                        .map_or(MethodPolicy::None, MethodPolicy::Callers),
                    // template:end service-auth:grpc-method-policy-service-tokens
                };
                (path.into_boxed_str(), kind, end_on_drain, policy)
            });
        let health = described_method_paths(&described, std::iter::once(HealthServer::NAME))
            .map(|(path, kind)| (path.into_boxed_str(), kind, false, MethodPolicy::None));
        let methods = Arc::new(Methods::new(business.chain(health)));
        Ok(BuiltServices {
            routes: routes.routes(),
            listener_layers,
            names,
            methods,
            authentication,
        })
    }

    pub(crate) fn attach_reflection(&mut self) -> Result<(), Error> {
        self.index_descriptor_services(v1::FILE_DESCRIPTOR_SET)?;
        self.index_descriptor_services(v1alpha::FILE_DESCRIPTOR_SET)?;
        let sets = REFLECTION_DESCRIPTORS
            .into_iter()
            .chain(self.descriptor_sets.iter().copied());
        reflection::check_files(sets.clone())?;
        let builder = || {
            let builder =
                tonic_reflection::server::Builder::configure().include_reflection_service(false);
            let builder = sets.clone().fold(
                builder,
                tonic_reflection::server::Builder::register_encoded_file_descriptor_set,
            );
            self.names
                .iter()
                .copied()
                .chain([
                    HealthServer::NAME,
                    REFLECTION_SERVICE,
                    v1alpha::server_reflection_server::SERVICE_NAME,
                ])
                .fold(
                    builder,
                    tonic_reflection::server::Builder::with_service_name,
                )
        };
        let v1 = builder()
            .build_v1()
            .map_err(|source| Error::new(ErrorKind::BuildReflectionV1(source)))?;
        let v1alpha = builder()
            .build_v1alpha()
            .map_err(|source| Error::new(ErrorKind::BuildReflectionV1Alpha(source)))?;
        let layer = MapRequestLayer::new(reflection::stop_request_body_on_cancellation);
        self.add(layer.named_layer(v1))?;
        self.add(layer.named_layer(v1alpha))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn into_ingress_parts(
        mut self,
    ) -> (
        axum::Router,
        Vec<ListenerLayer>,
        BTreeSet<&'static str>,
        Arc<Methods>,
    ) {
        self.attach_reflection().unwrap();
        self.index_descriptor_services(tonic_health::pb::FILE_DESCRIPTOR_SET)
            .unwrap();
        let services = self.names.iter().copied().chain([HealthServer::NAME]);
        let methods = Arc::new(Methods::new(
            described_method_paths(&self.described, services).map(|(path, kind)| {
                let end_on_drain = self.reflection && REFLECTION_PATHS.contains(&path.as_str());
                (
                    path.into_boxed_str(),
                    kind,
                    end_on_drain,
                    MethodPolicy::None,
                )
            }),
        ));
        (
            self.routes.routes().into_axum_router(),
            self.listener_layers,
            self.names,
            methods,
        )
    }

    #[allow(
        dead_code,
        reason = "only the scope and caller rules read it; the initializer removes both without bearer or service authentication"
    )]
    fn resolve_service_method(
        &self,
        service_name: &'static str,
        method: &str,
        policy: crate::error::PolicyKind,
    ) -> Result<String, Error> {
        let path = method_path(service_name, method);
        let known = !method.contains('/')
            && self
                .described
                .get(service_name)
                .is_some_and(|methods| methods.contains_key(method));
        if known {
            Ok(path)
        } else {
            Err(Error::new(ErrorKind::UnregisteredMethod { policy, path }))
        }
    }

    #[allow(
        dead_code,
        reason = "the initializer removes both policy checks without bearer or service authentication"
    )]
    fn first_method_without(&self, has_rule: impl Fn(&str) -> bool) -> Option<String> {
        described_method_paths(&self.described, self.names.iter().copied())
            .map(|(path, _)| path)
            .find(|path| !has_rule(path))
    }

    // template:begin authn:grpc-services-scope-helpers
    fn register_scopes_for_paths(
        &mut self,
        paths: &[&str],
        scopes: &[&str],
        delegate: bool,
    ) -> Result<(), Error> {
        let Some(&first) = paths.first() else {
            return Ok(());
        };
        if let Some(&duplicate) = paths.iter().find(|&&path| self.scopes.contains_key(path)) {
            return Err(Error::new(ErrorKind::DuplicatePolicy {
                policy: crate::error::PolicyKind::ScopeRequirement,
                path: duplicate.to_owned(),
            }));
        }
        validate_scope_tokens(first, scopes)?;
        let scopes: Box<[String]> = scopes.iter().map(|&scope| scope.to_owned()).collect();
        for &path in paths {
            self.scopes.insert(
                path.to_owned(),
                crate::authn::ScopeRequirement {
                    scopes: scopes.clone(),
                    delegate,
                },
            );
        }
        Ok(())
    }

    fn prepare_scope_requirements(&mut self) -> Result<(), Error> {
        if self.reflection {
            for path in REFLECTION_PATHS {
                self.scopes.entry(path.to_owned()).or_default();
            }
        }
        if let Some(path) = self.first_method_without(|path| self.scopes.contains_key(path)) {
            return Err(Error::new(ErrorKind::MissingPolicy {
                policy: crate::error::PolicyKind::ScopeRequirement,
                path,
            }));
        }
        Ok(())
    }
    // template:end authn:grpc-services-scope-helpers

    // template:begin service-auth:grpc-services-caller-helpers
    fn register_caller_rules_for_paths(
        &mut self,
        paths: &[&str],
        callers: &[&str],
    ) -> Result<(), Error> {
        let Some(&first) = paths.first() else {
            return Ok(());
        };
        if let Some(&duplicate) = paths
            .iter()
            .find(|&&path| self.caller_rules.contains_key(path))
        {
            return Err(Error::new(ErrorKind::DuplicatePolicy {
                policy: crate::error::PolicyKind::CallerRule,
                path: duplicate.to_owned(),
            }));
        }
        let names = parse_caller_names(first, callers)?;
        for &path in paths {
            self.caller_rules.insert(path.to_owned(), names.clone());
        }
        Ok(())
    }

    fn validate_caller_rules(&self) -> Result<(), Error> {
        if let Some(path) = self.first_method_without(|path| self.caller_rules.contains_key(path)) {
            return Err(Error::new(ErrorKind::MissingPolicy {
                policy: crate::error::PolicyKind::CallerRule,
                path,
            }));
        }
        Ok(())
    }
    // template:end service-auth:grpc-services-caller-helpers
}

fn described_method_paths(
    described: &HashMap<String, BTreeMap<String, CallType>>,
    services: impl Iterator<Item = &'static str>,
) -> impl Iterator<Item = (String, CallType)> {
    services.flat_map(|service| {
        described
            .get(service)
            .into_iter()
            .flatten()
            .map(move |(method, call_type)| (method_path(service, method), *call_type))
    })
}

// template:begin authn:grpc-services-validate-scope-tokens
fn validate_scope_tokens(path: &str, scopes: &[&str]) -> Result<(), Error> {
    if let Some(index) = scopes
        .iter()
        .position(|scope| !shared_rules::rfc6749::is_scope_token(scope))
    {
        return Err(Error::new(ErrorKind::InvalidScopeToken {
            path: path.to_owned(),
            index,
        }));
    }
    Ok(())
}
// template:end authn:grpc-services-validate-scope-tokens

// template:begin service-auth:grpc-services-parse-caller-names
fn parse_caller_names(
    path: &str,
    callers: &[&str],
) -> Result<foldhash::HashSet<infra_serviceauth::ServiceName>, Error> {
    let mut names = foldhash::HashSet::with_capacity_and_hasher(
        callers.len(),
        foldhash::fast::RandomState::default(),
    );
    for caller in callers {
        let name = infra_serviceauth::ServiceName::parse(caller).map_err(|source| {
            Error::new(ErrorKind::InvalidCallerRule {
                path: path.to_owned(),
                source,
            })
        })?;
        if !names.insert(name) {
            return Err(Error::new(ErrorKind::DuplicateCallerName(path.to_owned())));
        }
    }
    Ok(names)
}
// template:end service-auth:grpc-services-parse-caller-names

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn described_contract_records_method_streaming_cardinalities() {
        let mut services = Services::new();
        services
            .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
            .unwrap();
        let echo = &services.described["example.v1.EchoService"];
        assert_eq!(echo.get("Unary"), Some(&CallType::Unary));
        assert_eq!(echo.get("ClientStream"), Some(&CallType::ClientStream));
        assert_eq!(echo.get("ServerStream"), Some(&CallType::ServerStream));
        assert_eq!(echo.get("BidiStream"), Some(&CallType::BidiStream));
    }

    #[test]
    fn duplicate_reflection_and_malformed_descriptors_return_typed_error_kinds() {
        let mut services = Services::new();
        std::assert_matches!(
            services.describe(b"\xff"),
            Err(Error(ErrorKind::DecodeDescriptorSet(_)))
        );
        services.add_reflection().unwrap();
        std::assert_matches!(
            services.add_reflection(),
            Err(Error(ErrorKind::DuplicateService(REFLECTION_SERVICE)))
        );
    }

    // template:begin authn:grpc-services-test-reflection-scopes
    #[test]
    fn reflection_scope_rules_apply_to_both_reflection_versions_and_reject_duplicates() {
        let mut services = Services::new();
        let mut reflection = services.add_reflection().unwrap();
        std::assert_matches!(
            reflection.require_scopes(&["bad scope"]),
            Err(Error(ErrorKind::InvalidScopeToken { path, index: 0 }))
                if path == REFLECTION_PATHS[0]
        );
        reflection.require_scopes(&["reflection.read"]).unwrap();
        std::assert_matches!(
            reflection.require_scopes(&["reflection.read"]),
            Err(Error(ErrorKind::DuplicatePolicy {
                policy: crate::error::PolicyKind::ScopeRequirement,
                path,
            })) if path == REFLECTION_PATHS[0]
        );
    }
    // template:end authn:grpc-services-test-reflection-scopes

    // template:begin service-auth:grpc-services-test-reflection-callers
    #[test]
    fn reflection_caller_rules_apply_to_both_reflection_versions_and_reject_duplicates() {
        let mut services = Services::new();
        let mut reflection = services.add_reflection().unwrap();
        std::assert_matches!(
            reflection.allow_callers(&["Orders"]),
            Err(Error(ErrorKind::InvalidCallerRule { path, .. })) if path == REFLECTION_PATHS[0]
        );
        std::assert_matches!(
            reflection.allow_callers(&["orders", "orders"]),
            Err(Error(ErrorKind::DuplicateCallerName(path))) if path == REFLECTION_PATHS[0]
        );
        reflection.allow_callers(&["orders"]).unwrap();
        std::assert_matches!(
            reflection.allow_callers(&["billing"]),
            Err(Error(ErrorKind::DuplicatePolicy {
                policy: crate::error::PolicyKind::CallerRule,
                path,
            })) if path == REFLECTION_PATHS[0]
        );
    }
    // template:end service-auth:grpc-services-test-reflection-callers
}
