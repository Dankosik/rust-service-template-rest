use std::collections::BTreeSet;
use std::convert::Infallible;

use foldhash::{HashMap, HashSet};
use prost::Message as _;
use tonic::server::NamedService as _;
use tonic_reflection::pb::{v1, v1alpha};

use crate::Error;
use crate::health::HealthServer;
use crate::observe::CallType;
use crate::protocol::{method_path, split_method_path};

const REFLECTION_SERVICE: &str = v1::server_reflection_server::SERVICE_NAME;

#[derive(Debug, Default)]
pub struct Services {
    pub(crate) routes: tonic::service::RoutesBuilder,
    pub(crate) names: BTreeSet<&'static str>,
    described: HashMap<String, Vec<(String, CallType)>>,
    descriptor_sets: Vec<&'static [u8]>,
    pub(crate) reflection: bool,
    // template:begin authn:grpc-services-scopes-field
    pub(crate) scopes: HashMap<String, Box<[String]>>,
    // template:end authn:grpc-services-scopes-field
    // template:begin service-auth:grpc-services-callers-field
    caller_rules: HashMap<String, Box<[infra_serviceauth::ServiceName]>>,
    caller_verifier: Option<infra_serviceauth::Verifier>,
    // template:end service-auth:grpc-services-callers-field
}

impl Services {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn describe(&mut self, encoded_file_descriptor_set: &'static [u8]) -> Result<(), Error> {
        self.learn(encoded_file_descriptor_set)?;
        self.descriptor_sets.push(encoded_file_descriptor_set);
        Ok(())
    }

    pub(crate) fn learn(&mut self, encoded_file_descriptor_set: &[u8]) -> Result<(), Error> {
        let set = prost_types::FileDescriptorSet::decode(encoded_file_descriptor_set)
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
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
        let builder = || {
            let builder = tonic_reflection::server::Builder::configure()
                .include_reflection_service(false)
                .register_encoded_file_descriptor_set(v1::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(v1alpha::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET);
            let builder = self.descriptor_sets.iter().fold(builder, |builder, set| {
                builder.register_encoded_file_descriptor_set(set)
            });
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
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        let v1alpha = builder()
            .build_v1alpha()
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        self.add(v1)?;
        self.add(v1alpha)
    }

    #[allow(
        dead_code,
        reason = "only the scope and caller rules read it; the initializer removes both without bearer or service authentication"
    )]
    fn is_registered_method(&self, path: &str) -> bool {
        split_method_path(path).is_some_and(|(service, method)| {
            self.names.contains(service)
                && self
                    .described
                    .get(service)
                    .is_some_and(|methods| methods.iter().any(|(known, _)| known == method))
        })
    }

    pub(crate) fn method_paths(&self) -> HashMap<Box<str>, CallType> {
        self.names
            .iter()
            .copied()
            .chain([HealthServer::NAME])
            .flat_map(|service| {
                let methods = self.described.get(service).into_iter().flatten();
                methods.map(move |(method, call_type)| {
                    (method_path(service, method).into_boxed_str(), *call_type)
                })
            })
            .collect()
    }

    // template:begin authn:grpc-services-require-scopes
    pub fn require_scopes(&mut self, path: &str, scopes: &[&str]) -> Result<(), Error> {
        if !self.is_registered_method(path) {
            return Err(Error::UnregisteredMethodPath(path.to_owned()));
        }
        if self.scopes.contains_key(path) {
            return Err(Error::DuplicateScopeRequirement(path.to_owned()));
        }
        let scopes = scopes.iter().map(|&scope| scope.to_owned()).collect();
        self.scopes.insert(path.to_owned(), scopes);
        Ok(())
    }
    // template:end authn:grpc-services-require-scopes

    // template:begin service-auth:grpc-services-allow-callers
    pub fn allow_callers(&mut self, path: &str, callers: &[&str]) -> Result<(), Error> {
        let reflection = self.reflection && crate::REFLECTION_PATHS.contains(&path);
        if !(self.is_registered_method(path) || reflection) {
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

    pub fn authenticate_callers(&mut self, verifier: infra_serviceauth::Verifier) {
        self.caller_verifier = Some(verifier);
    }

    pub(crate) fn take_caller_admission(
        &mut self,
    ) -> Result<crate::service_auth::CallerAdmission, Error> {
        if self.caller_verifier.is_some() || !self.caller_rules.is_empty() {
            for service in &self.names {
                for (method, _) in self.described.get(*service).into_iter().flatten() {
                    let path = method_path(service, method);
                    if !self.caller_rules.contains_key(&path) {
                        return Err(Error::MissingCallerRule(path));
                    }
                }
            }
        }
        Ok(crate::service_auth::CallerAdmission::new(
            self.caller_verifier.take(),
            std::mem::take(&mut self.caller_rules),
        ))
    }
    // template:end service-auth:grpc-services-allow-callers
}
