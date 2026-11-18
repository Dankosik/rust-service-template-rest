use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::{Arc, OnceLock};

use foldhash::{HashMap, HashSet};
use prost::Message as _;
use tonic::server::NamedService as _;
use tonic::service::LayerExt as _;
use tonic_reflection::pb::{v1, v1alpha};
use tower::util::{BoxCloneSyncServiceLayer, MapRequestLayer};
use tower::{Layer, Service};

use crate::Error;
use crate::health::HealthServer;
use crate::observe::MethodMetrics;
use crate::protocol::{method_path, split_method_path};

mod reflection;

const REFLECTION_SERVICE: &str = v1::server_reflection_server::SERVICE_NAME;
const REFLECTION_DESCRIPTORS: [&[u8]; 3] = [
    v1::FILE_DESCRIPTOR_SET,
    v1alpha::FILE_DESCRIPTOR_SET,
    tonic_health::pb::FILE_DESCRIPTOR_SET,
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

pub const REFLECTION_PATHS: [&str; 2] = [
    "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
];

pub(crate) type ListenerLayer = BoxCloneSyncServiceLayer<
    axum::Router,
    axum::extract::Request,
    axum::response::Response,
    Infallible,
>;

#[derive(Debug, Default)]
pub struct Services {
    pub(crate) listener_layer: Option<ListenerLayer>,
    pub(crate) routes: tonic::service::RoutesBuilder,
    pub(crate) names: BTreeSet<&'static str>,
    described: HashMap<String, Vec<(String, CallType)>>,
    descriptor_sets: Vec<&'static [u8]>,
    pub(crate) reflection: bool,
    // template:begin authn:grpc-services-scopes-field
    pub(crate) scopes: crate::authn::Scopes,
    // template:end authn:grpc-services-scopes-field
    // template:begin service-auth:grpc-services-callers-field
    pub(crate) caller_rules: crate::service_auth::CallerRules,
    // template:end service-auth:grpc-services-callers-field
}

impl Services {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_listener_layer<L>(&mut self, layer: L)
    where
        L: Layer<axum::Router> + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request, Response = axum::response::Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.listener_layer = Some(ListenerLayer::new(layer));
    }

    pub fn describe(&mut self, encoded_file_descriptor_set: &'static [u8]) -> Result<(), Error> {
        self.learn(encoded_file_descriptor_set)?;
        self.descriptor_sets.push(encoded_file_descriptor_set);
        Ok(())
    }

    pub(crate) fn learn(&mut self, encoded_file_descriptor_set: &[u8]) -> Result<(), Error> {
        let set = prost_types::FileDescriptorSet::decode(encoded_file_descriptor_set)
            .map_err(Error::DecodeDescriptorSet)?;
        let mut described: HashMap<String, Vec<(String, CallType)>> = HashMap::default();
        for file in set.file {
            let package = file.package.as_deref().unwrap_or_default();
            for service in file.service {
                let name = service.name.unwrap_or_default();
                let name = if package.is_empty() {
                    name
                } else {
                    format!("{package}.{name}")
                };
                let methods: Vec<_> = service
                    .method
                    .into_iter()
                    .map(|method| {
                        let call_type =
                            CallType::new(method.client_streaming(), method.server_streaming());
                        (method.name.unwrap_or_default(), call_type)
                    })
                    .collect();
                if let Some(previous) = described.get(&name).or_else(|| self.described.get(&name)) {
                    if previous != &methods
                        && previous.iter().collect::<HashSet<_>>()
                            != methods.iter().collect::<HashSet<_>>()
                    {
                        return Err(Error::ConflictingServiceDescription(name));
                    }
                } else {
                    described.insert(name, methods);
                }
            }
        }
        self.described.extend(described);
        Ok(())
    }

    pub fn add<S>(&mut self, service: S) -> Result<(), Error>
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
        Ok(())
    }

    pub fn add_reflection(&mut self) -> Result<(), Error> {
        if std::mem::replace(&mut self.reflection, true) {
            return Err(Error::DuplicateService(REFLECTION_SERVICE));
        }
        Ok(())
    }

    pub(crate) fn attach_reflection(&mut self) -> Result<(), Error> {
        self.learn(v1::FILE_DESCRIPTOR_SET)?;
        self.learn(v1alpha::FILE_DESCRIPTOR_SET)?;
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
        let layer = MapRequestLayer::new(reflection::cancel_request_body);
        self.add(layer.named_layer(v1))?;
        self.add(layer.named_layer(v1alpha))
    }

    #[allow(
        dead_code,
        reason = "only the scope and caller rules read it; the initializer removes both without bearer or service authentication"
    )]
    fn is_registered_method(&self, path: &str) -> bool {
        if self.reflection && REFLECTION_PATHS.contains(&path) {
            return true;
        }
        split_method_path(path).is_some_and(|(service, method)| {
            self.names.contains(service)
                && self
                    .described
                    .get(service)
                    .is_some_and(|methods| methods.iter().any(|(known, _)| known == method))
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

    // template:begin authn:grpc-services-require-scopes
    pub fn require_scopes(&mut self, path: &str, scopes: &[&str]) -> Result<(), Error> {
        self.register_scopes(path, scopes, false)
    }

    pub fn require_scopes_for_delegation(
        &mut self,
        path: &str,
        scopes: &[&str],
    ) -> Result<(), Error> {
        self.register_scopes(path, scopes, true)
    }

    fn register_scopes(
        &mut self,
        path: &str,
        scopes: &[&str],
        delegate: bool,
    ) -> Result<(), Error> {
        if !self.is_registered_method(path) {
            return Err(Error::UnregisteredMethodPath(path.to_owned()));
        }
        if self.scopes.contains_key(path) {
            return Err(Error::DuplicateScopeRequirement(path.to_owned()));
        }
        if let Some(index) = scopes
            .iter()
            .position(|scope| !shared_rules::rfc6749::is_scope_token(scope))
        {
            return Err(Error::InvalidScopeToken {
                path: path.to_owned(),
                index,
            });
        }
        let scopes = scopes.iter().map(|&scope| scope.to_owned()).collect();
        self.scopes.insert(
            path.to_owned(),
            crate::authn::ScopeRequirement { scopes, delegate },
        );
        Ok(())
    }

    pub(crate) fn prepare_scope_requirements(&mut self) -> Result<(), Error> {
        if self.reflection {
            for path in REFLECTION_PATHS {
                self.scopes.entry(path.to_owned()).or_default();
            }
        }
        if let Some((path, _)) = self
            .method_paths(self.names.iter().copied())
            .find(|(path, _)| !self.scopes.contains_key(path))
        {
            return Err(Error::MissingScopeRequirement(path));
        }
        Ok(())
    }
    // template:end authn:grpc-services-require-scopes

    // template:begin service-auth:grpc-services-allow-callers
    pub fn allow_callers(&mut self, path: &str, callers: &[&str]) -> Result<(), Error> {
        if !self.is_registered_method(path) {
            return Err(Error::UnregisteredCallerRule(path.to_owned()));
        }
        if self.caller_rules.contains_key(path) {
            return Err(Error::DuplicateCallerRule(path.to_owned()));
        }
        let mut names: Vec<infra_serviceauth::ServiceName> = Vec::with_capacity(callers.len());
        for caller in callers {
            let name = infra_serviceauth::ServiceName::parse(caller).map_err(|source| {
                Error::InvalidCallerRule {
                    path: path.to_owned(),
                    source,
                }
            })?;
            if names.contains(&name) {
                return Err(Error::DuplicateCallerName(path.to_owned()));
            }
            names.push(name);
        }
        self.caller_rules
            .insert(path.to_owned(), names.into_boxed_slice());
        Ok(())
    }

    pub(crate) fn validate_caller_rules(&self) -> Result<(), Error> {
        if let Some((path, _)) = self
            .method_paths(self.names.iter().copied())
            .find(|(path, _)| !self.caller_rules.contains_key(path))
        {
            return Err(Error::MissingCallerRule(path));
        }
        Ok(())
    }
    // template:end service-auth:grpc-services-allow-callers
}

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
        assert!(echo.contains(&("Unary".to_owned(), CallType::Unary)));
        assert!(echo.contains(&("ClientStream".to_owned(), CallType::ClientStream)));
        assert!(echo.contains(&("ServerStream".to_owned(), CallType::ServerStream)));
        assert!(echo.contains(&("BidiStream".to_owned(), CallType::BidiStream)));
    }

    // template:begin authn:grpc-services-test-reflection-scopes
    #[test]
    fn reflection_scope_rules_require_registration_and_an_exact_rpc_path() {
        let mut services = Services::new();
        let paths = [
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
            "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
        ];
        for path in paths {
            std::assert_matches!(
                services.require_scopes(path, &["reflection.read"]),
                Err(Error::UnregisteredMethodPath(rejected)) if rejected == path
            );
        }
        services.add_reflection().unwrap();
        for path in paths {
            services.require_scopes(path, &["reflection.read"]).unwrap();
        }
        let unknown = "/grpc.reflection.v1.ServerReflection/Unknown";
        std::assert_matches!(
            services.require_scopes(unknown, &["reflection.read"]),
            Err(Error::UnregisteredMethodPath(rejected)) if rejected == unknown
        );
    }
    // template:end authn:grpc-services-test-reflection-scopes
}
