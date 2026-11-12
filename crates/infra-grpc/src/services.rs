use std::collections::BTreeSet;
use std::convert::Infallible;

use foldhash::{HashMap, HashSet};
use prost::Message as _;
use tonic::server::NamedService as _;

use crate::Error;
use crate::health::HealthServer;
use crate::protocol::{method_path, split_method_path};

const REFLECTION_SERVICE: &str = tonic_reflection::pb::v1::server_reflection_server::SERVICE_NAME;

/// Registered generated services and the contracts that describe them.
/// Health is attached later, outside the business concurrency limit and
/// authentication.
#[derive(Debug, Default)]
pub struct Services {
    pub(crate) routes: tonic::service::RoutesBuilder,
    pub(crate) names: BTreeSet<&'static str>,
    /// Method names of every described service, by full service name.
    described: HashMap<String, Vec<String>>,
    /// The sets given to [`Services::describe`], which reflection serves.
    descriptor_sets: Vec<&'static [u8]>,
    pub(crate) reflection: bool,
    // template:begin authn:grpc-services-scopes-field
    /// Scopes a verified principal needs for a method, keyed by request path.
    pub(crate) scopes: HashMap<String, Box<[String]>>,
    // template:end authn:grpc-services-scopes-field
    // template:begin service-auth:grpc-services-callers-field
    /// Callers admitted to a method, keyed by request path.
    caller_rules: HashMap<String, Box<[infra_serviceauth::CallerName]>>,
    /// Verifies the service token of each business call, when set.
    caller_verifier: Option<infra_serviceauth::Verifier>,
    // template:end service-auth:grpc-services-callers-field
}

impl Services {
    /// An empty registry. Health is still served.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the services and methods of an encoded `FileDescriptorSet`,
    /// such as `grpc_contracts::FILE_DESCRIPTOR_SET`. A generated server does
    /// not list its methods; the set is what lets a scope requirement name a
    /// real method and a metric carry a method label no caller can invent.
    /// The set itself is kept for [`Services::add_reflection`] to serve, hence
    /// `'static`. Describe a contract before adding its servers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFileDescriptorSet`] when the bytes do not
    /// decode.
    pub fn describe(&mut self, encoded_file_descriptor_set: &'static [u8]) -> Result<(), Error> {
        self.learn(encoded_file_descriptor_set)?;
        self.descriptor_sets.push(encoded_file_descriptor_set);
        Ok(())
    }

    pub(crate) fn learn(&mut self, encoded_file_descriptor_set: &[u8]) -> Result<(), Error> {
        let set = prost_types::FileDescriptorSet::decode(encoded_file_descriptor_set)
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        for file in &set.file {
            for service in &file.service {
                let name = match file.package() {
                    "" => service.name().to_owned(),
                    package => format!("{package}.{}", service.name()),
                };
                let methods = service
                    .method
                    .iter()
                    .map(|method| method.name().to_owned())
                    .collect();
                self.described.insert(name, methods);
            }
        }
        Ok(())
    }

    /// Records `S::NAME` and adds the generated server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UndescribedService`] when no described set holds
    /// that name, and [`Error::DuplicateService`] when it is already
    /// registered.
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
        if !self.described.contains_key(S::NAME) {
            return Err(Error::UndescribedService(S::NAME));
        }
        if !self.names.insert(S::NAME) {
            return Err(Error::DuplicateService(S::NAME));
        }
        self.routes.add_service(service);
        Ok(())
    }

    /// Adds standard server reflection over every described set, whenever
    /// it was described, so `grpcurl` and `buf curl` can call the service
    /// without its schema files. Both protocol versions are served,
    /// `grpc.reflection.v1` and the `v1alpha` it replaced, as grpc-go serves
    /// them: older tools ask only for `v1alpha`. Health is described too.
    /// Reflection is a business route: admitted, limited and deadline-bound
    /// like every other registered service.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DuplicateService`] when reflection is already added.
    pub fn add_reflection(&mut self) -> Result<(), Error> {
        if std::mem::replace(&mut self.reflection, true) {
            return Err(Error::DuplicateService(REFLECTION_SERVICE));
        }
        Ok(())
    }

    /// Builds the reflection servers requested by [`Services::add_reflection`].
    pub(crate) fn attach_reflection(&mut self) -> Result<(), Error> {
        self.learn(tonic_reflection::pb::v1::FILE_DESCRIPTOR_SET)?;
        self.learn(tonic_reflection::pb::v1alpha::FILE_DESCRIPTOR_SET)?;
        // Each version lists the other, as grpc-go's registration does;
        // the builder would add only the one it builds.
        let builder = || {
            let builder = tonic_reflection::server::Builder::configure()
                .include_reflection_service(false)
                .register_encoded_file_descriptor_set(tonic_reflection::pb::v1::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(
                    tonic_reflection::pb::v1alpha::FILE_DESCRIPTOR_SET,
                )
                .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET);
            self.descriptor_sets.iter().fold(builder, |builder, set| {
                builder.register_encoded_file_descriptor_set(set)
            })
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

    /// Whether `path` is `/{service}/{method}` of a described method of an
    /// added service.
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
                    .is_some_and(|methods| methods.iter().any(|known| known == method))
        })
    }

    /// The request path of every described method of the registered
    /// services and of health: the only paths that become metric labels.
    pub(crate) fn method_paths(&self) -> HashSet<Box<str>> {
        self.names
            .iter()
            .copied()
            .chain([HealthServer::NAME])
            .flat_map(|service| {
                let methods = self.described.get(service).into_iter().flatten();
                methods.map(move |method| method_path(service, method).into_boxed_str())
            })
            .collect()
    }

    // template:begin authn:grpc-services-require-scopes
    /// Requires every scope in `scopes` for calls to `path`, the method's
    /// request path such as `/example.v1.EchoService/Unary`. A principal that
    /// lacks one gets `PERMISSION_DENIED` before the handler. A method with
    /// no declared requirement admits any authenticated caller, as an OpenAPI
    /// operation without scopes does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnregisteredMethodPath`] unless `path` is
    /// `/{service}/{method}` of a described method of a service already
    /// added, and [`Error::DuplicateScopeRequirement`] when `path` already
    /// has one.
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
    /// Admits only `callers`, by service name, to `path`, the method's
    /// request path such as `/example.v1.EchoService/Unary`; an empty list
    /// admits nobody. Every method of every added service needs a rule, and
    /// so does each of [`REFLECTION_PATHS`](crate::REFLECTION_PATHS) once
    /// reflection is added. Rules are enforced only with
    /// [`Services::authenticate_callers`]; without it, [`router`](crate::router()) checks only
    /// that a declared table is complete and refuses to build otherwise. A
    /// caller outside the rule gets `PERMISSION_DENIED` before the handler.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnregisteredCallerRule`] unless `path` names a
    /// described method of an added service or an added reflection path,
    /// [`Error::DuplicateCallerRule`] when `path` already has a rule, and
    /// [`Error::InvalidCallerRule`] for a caller outside the name grammar or
    /// named twice.
    pub fn allow_callers(&mut self, path: &str, callers: &[&str]) -> Result<(), Error> {
        let reflection = self.reflection && crate::REFLECTION_PATHS.contains(&path);
        if !(self.is_registered_method(path) || reflection) {
            return Err(Error::UnregisteredCallerRule(path.to_owned()));
        }
        if self.caller_rules.contains_key(path) {
            return Err(Error::DuplicateCallerRule(path.to_owned()));
        }
        let mut names: Vec<infra_serviceauth::CallerName> = Vec::with_capacity(callers.len());
        for caller in callers {
            let name = infra_serviceauth::CallerName::parse(caller)
                .map_err(|_| Error::InvalidCallerRule(path.to_owned()))?;
            if names.contains(&name) {
                return Err(Error::InvalidCallerRule(path.to_owned()));
            }
            names.push(name);
        }
        self.caller_rules
            .insert(path.to_owned(), names.into_boxed_slice());
        Ok(())
    }

    /// Authenticates business calls with the service tokens `verifier`
    /// accepts and admits each caller by its [rule](Self::allow_callers).
    /// On this listener it replaces bearer authentication. Without it,
    /// [`router`](crate::router()) authenticates with the bearer verifier where that profile
    /// is compiled in, and refuses to build when none is given; a build
    /// without bearer authentication serves business calls without a token,
    /// which configuration permits only in a development environment.
    pub fn authenticate_callers(&mut self, verifier: infra_serviceauth::Verifier) {
        self.caller_verifier = Some(verifier);
    }

    /// The permission table, complete whenever service tokens are verified
    /// or any rule is declared: every method of every added service,
    /// reflection included, then has a rule. A registration that declares
    /// none and verifies no token is left to bearer authentication or, in
    /// development, to no authentication.
    pub(crate) fn caller_admission(
        &mut self,
    ) -> Result<crate::service_auth::CallerAdmission, Error> {
        if self.caller_verifier.is_some() || !self.caller_rules.is_empty() {
            for service in &self.names {
                for method in self.described.get(*service).into_iter().flatten() {
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
