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

use crate::health::HealthServer;
use crate::observe::MethodMetrics;
use crate::protocol::{method_path, split_method_path};
use crate::router::{Application, Authentication, BusinessAuth, ServerOptions};
#[allow(
    unused_imports,
    reason = "optional auth profiles use PolicyKind when registering method policies"
)]
use crate::{Error, PolicyKind};

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

#[derive(Default)]
pub(crate) struct Method {
    pub(crate) call_type: Option<CallType>,
    pub(crate) metrics: OnceLock<Arc<MethodMetrics>>,
}

pub(crate) struct Methods {
    registered: HashMap<Box<str>, Method>,
    unknown: Method,
}

impl Methods {
    pub(crate) fn new(paths: impl IntoIterator<Item = (Box<str>, CallType)>) -> Self {
        Self {
            registered: paths
                .into_iter()
                .map(|(path, call_type)| {
                    (
                        path,
                        Method {
                            call_type: Some(call_type),
                            metrics: OnceLock::new(),
                        },
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

pub type ListenerService =
    BoxCloneSyncService<axum::extract::Request, axum::response::Response, Infallible>;

pub(crate) type ListenerLayer = BoxCloneSyncServiceLayer<
    ListenerService,
    axum::extract::Request,
    axum::response::Response,
    Infallible,
>;

pub(crate) struct BuiltServices {
    pub(crate) routes: tonic::service::Routes,
    pub(crate) listener_layers: Vec<ListenerLayer>,
    pub(crate) names: BTreeSet<&'static str>,
    pub(crate) methods: Arc<Methods>,
    pub(crate) reflection: bool,
    pub(crate) authentication: BusinessAuth,
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
    scopes: crate::authn::Scopes,
    // template:end authn:grpc-services-scopes-field
    // template:begin service-auth:grpc-services-callers-field
    caller_rules: crate::service_auth::CallerRules,
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
        let path = method_path(self.service_name, method);
        self.services.register_scopes(&path, scopes, false)?;
        Ok(self)
    }

    pub fn require_scopes_for_delegation(
        &mut self,
        method: &str,
        scopes: &[&str],
    ) -> Result<&mut Self, Error> {
        let path = method_path(self.service_name, method);
        self.services.register_scopes(&path, scopes, true)?;
        Ok(self)
    }
    // template:end authn:grpc-services-require-scopes

    // template:begin service-auth:grpc-services-allow-callers
    pub fn allow_callers(&mut self, method: &str, callers: &[&str]) -> Result<&mut Self, Error> {
        let path = method_path(self.service_name, method);
        self.services.register_caller_rule(&path, callers)?;
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
        self.register_scopes(scopes, false)
    }

    pub fn require_scopes_for_delegation(&mut self, scopes: &[&str]) -> Result<&mut Self, Error> {
        self.register_scopes(scopes, true)
    }

    fn register_scopes(&mut self, scopes: &[&str], delegate: bool) -> Result<&mut Self, Error> {
        for path in REFLECTION_PATHS {
            if self.services.scopes.contains_key(path) {
                return Err(Error::DuplicatePolicy {
                    policy: PolicyKind::ScopeRequirement,
                    path: path.to_owned(),
                });
            }
            validate_scope_tokens(path, scopes)?;
        }
        for path in REFLECTION_PATHS {
            self.services.register_scopes(path, scopes, delegate)?;
        }
        Ok(self)
    }
    // template:end authn:grpc-reflection-require-scopes

    // template:begin service-auth:grpc-reflection-allow-callers
    pub fn allow_callers(&mut self, callers: &[&str]) -> Result<&mut Self, Error> {
        let mut parsed = Vec::with_capacity(REFLECTION_PATHS.len());
        for path in REFLECTION_PATHS {
            if self.services.caller_rules.contains_key(path) {
                return Err(Error::DuplicatePolicy {
                    policy: PolicyKind::CallerRule,
                    path: path.to_owned(),
                });
            }
            parsed.push((path, parse_caller_names(path, callers)?));
        }
        for (path, names) in parsed {
            self.services.caller_rules.insert(path.to_owned(), names);
        }
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
        L::Service: Service<axum::extract::Request, Response = axum::response::Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static,
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
            .map_err(Error::DecodeDescriptorSet)?;
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
                        return Err(Error::ConflictingServiceDescription(name));
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
            return Err(Error::DuplicateService(S::NAME));
        }
        if !self.described.contains_key(S::NAME) {
            return Err(Error::UndescribedService(S::NAME));
        }
        if !self.names.insert(S::NAME) {
            return Err(Error::DuplicateService(S::NAME));
        }
        self.routes.add_service(service);
        Ok(ServiceRegistration {
            services: self,
            service_name: S::NAME,
        })
    }

    pub fn add_reflection(&mut self) -> Result<ReflectionRegistration<'_>, Error> {
        if std::mem::replace(&mut self.reflection, true) {
            return Err(Error::DuplicateService(REFLECTION_SERVICE));
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
        let methods = Arc::new(self.methods());
        // template:begin service-auth:grpc-router-caller-rules
        if matches!(authentication, Authentication::ServiceTokens(_)) {
            self.validate_caller_rules()?;
        } else if !self.caller_rules.is_empty() {
            return Err(Error::CallerRulesRequireServiceTokens);
        }
        // template:end service-auth:grpc-router-caller-rules
        // template:begin authn:grpc-router-authenticate
        if matches!(authentication, Authentication::None) {
            return Err(Error::UnauthenticatedBusinessCalls);
        }
        if matches!(authentication, Authentication::Bearer(_)) {
            self.prepare_scope_requirements()?;
        } else if self
            .scopes
            .values()
            .any(|rule| rule.delegate || !rule.scopes.is_empty())
        {
            return Err(Error::ScopesWithServiceTokens);
        }
        // template:end authn:grpc-router-authenticate
        let Self {
            listener_layers,
            routes,
            names,
            reflection,
            // template:begin authn:grpc-router-bearer-destructure
            scopes,
            // template:end authn:grpc-router-bearer-destructure
            // template:begin service-auth:grpc-router-caller-destructure
            caller_rules,
            // template:end service-auth:grpc-router-caller-destructure
            ..
        } = self;
        let authentication = match authentication {
            Authentication::None => BusinessAuth::None,
            // template:begin authn:grpc-router-bearer-init
            Authentication::Bearer(verifier) => BusinessAuth::Bearer { verifier, scopes },
            // template:end authn:grpc-router-bearer-init
            // template:begin service-auth:grpc-router-caller-init
            Authentication::ServiceTokens(verifier) => BusinessAuth::ServiceTokens {
                verifier,
                caller_rules,
            },
            // template:end service-auth:grpc-router-caller-init
        };
        Ok(BuiltServices {
            routes: routes.routes(),
            listener_layers,
            names,
            methods,
            reflection,
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
        let v1 = builder().build_v1().map_err(Error::BuildReflectionV1)?;
        let v1alpha = builder()
            .build_v1alpha()
            .map_err(Error::BuildReflectionV1Alpha)?;
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
        let methods = Arc::new(self.methods());
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
    fn is_registered_method(&self, path: &str) -> bool {
        if self.reflection && REFLECTION_PATHS.contains(&path) {
            return true;
        }
        split_method_path(path).is_some_and(|rpc| {
            self.names.contains(rpc.service)
                && self
                    .described
                    .get(rpc.service)
                    .is_some_and(|methods| methods.contains_key(rpc.method))
        })
    }

    fn method_paths(
        &self,
        services: impl Iterator<Item = &'static str>,
    ) -> impl Iterator<Item = (String, CallType)> {
        services.flat_map(|service| {
            self.described
                .get(service)
                .into_iter()
                .flatten()
                .map(move |(method, call_type)| (method_path(service, method), *call_type))
        })
    }

    pub(crate) fn methods(&self) -> Methods {
        let services = self.names.iter().copied().chain([HealthServer::NAME]);
        Methods::new(
            self.method_paths(services)
                .map(|(path, kind)| (path.into_boxed_str(), kind)),
        )
    }

    #[allow(
        dead_code,
        reason = "the initializer removes both policy checks without bearer or service authentication"
    )]
    fn first_method_without(&self, has_rule: impl Fn(&str) -> bool) -> Option<String> {
        self.method_paths(self.names.iter().copied())
            .map(|(path, _)| path)
            .find(|path| !has_rule(path))
    }

    // template:begin authn:grpc-services-scope-helpers
    fn register_scopes(
        &mut self,
        path: &str,
        scopes: &[&str],
        delegate: bool,
    ) -> Result<(), Error> {
        if !self.is_registered_method(path) {
            return Err(Error::UnregisteredMethod {
                policy: PolicyKind::ScopeRequirement,
                path: path.to_owned(),
            });
        }
        if self.scopes.contains_key(path) {
            return Err(Error::DuplicatePolicy {
                policy: PolicyKind::ScopeRequirement,
                path: path.to_owned(),
            });
        }
        validate_scope_tokens(path, scopes)?;
        let scopes = scopes.iter().map(|&scope| scope.to_owned()).collect();
        self.scopes.insert(
            path.to_owned(),
            crate::authn::ScopeRequirement { scopes, delegate },
        );
        Ok(())
    }

    fn prepare_scope_requirements(&mut self) -> Result<(), Error> {
        if self.reflection {
            for path in REFLECTION_PATHS {
                self.scopes.entry(path.to_owned()).or_default();
            }
        }
        if let Some(path) = self.first_method_without(|path| self.scopes.contains_key(path)) {
            return Err(Error::MissingPolicy {
                policy: PolicyKind::ScopeRequirement,
                path,
            });
        }
        Ok(())
    }
    // template:end authn:grpc-services-scope-helpers

    // template:begin service-auth:grpc-services-caller-helpers
    fn register_caller_rule(&mut self, path: &str, callers: &[&str]) -> Result<(), Error> {
        if !self.is_registered_method(path) {
            return Err(Error::UnregisteredMethod {
                policy: PolicyKind::CallerRule,
                path: path.to_owned(),
            });
        }
        if self.caller_rules.contains_key(path) {
            return Err(Error::DuplicatePolicy {
                policy: PolicyKind::CallerRule,
                path: path.to_owned(),
            });
        }
        let names = parse_caller_names(path, callers)?;
        self.caller_rules.insert(path.to_owned(), names);
        Ok(())
    }

    fn validate_caller_rules(&self) -> Result<(), Error> {
        if let Some(path) = self.first_method_without(|path| self.caller_rules.contains_key(path)) {
            return Err(Error::MissingPolicy {
                policy: PolicyKind::CallerRule,
                path,
            });
        }
        Ok(())
    }
    // template:end service-auth:grpc-services-caller-helpers
}

// template:begin authn:grpc-services-validate-scope-tokens
fn validate_scope_tokens(path: &str, scopes: &[&str]) -> Result<(), Error> {
    if let Some(index) = scopes
        .iter()
        .position(|scope| !shared_rules::rfc6749::is_scope_token(scope))
    {
        return Err(Error::InvalidScopeToken {
            path: path.to_owned(),
            index,
        });
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
            Error::InvalidCallerRule {
                path: path.to_owned(),
                source,
            }
        })?;
        if !names.insert(name) {
            return Err(Error::DuplicateCallerName(path.to_owned()));
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

    // template:begin authn:grpc-services-test-reflection-scopes
    #[test]
    fn reflection_scope_rules_apply_to_both_reflection_versions_and_reject_duplicates() {
        let mut services = Services::new();
        let mut reflection = services.add_reflection().unwrap();
        std::assert_matches!(
            reflection.require_scopes(&["bad scope"]),
            Err(Error::InvalidScopeToken { path, index: 0 }) if path == REFLECTION_PATHS[0]
        );
        reflection.require_scopes(&["reflection.read"]).unwrap();
        std::assert_matches!(
            reflection.require_scopes(&["reflection.read"]),
            Err(Error::DuplicatePolicy {
                policy: PolicyKind::ScopeRequirement,
                path,
            }) if path == REFLECTION_PATHS[0]
        );
    }
    // template:end authn:grpc-services-test-reflection-scopes
}
