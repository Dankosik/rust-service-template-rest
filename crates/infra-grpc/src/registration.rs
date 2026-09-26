use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use http::Request;
use tonic::body::Body;
use tonic::server::NamedService;
use tonic::service::Routes;
use tower::Service;

use crate::Error;
use crate::validation::Validation;

/// The four native RPC cardinalities supported by the governed generator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Cardinality {
    Unary,
    ClientStreaming,
    ServerStreaming,
    BidiStreaming,
}

/// One schema-owned RPC method.
///
/// The input and output names are descriptor identities, not peer supplied
/// path values.  The generator emits these from the committed descriptor set.
#[derive(Clone, Copy, Debug)]
pub struct Method {
    path: &'static str,
    request_type: &'static str,
    response_type: &'static str,
    cardinality: Cardinality,
    request_descriptor: fn() -> prost_reflect::MessageDescriptor,
}

impl Method {
    #[must_use]
    pub const fn new(
        path: &'static str,
        request_type: &'static str,
        response_type: &'static str,
        cardinality: Cardinality,
        request_descriptor: fn() -> prost_reflect::MessageDescriptor,
    ) -> Self {
        Self {
            path,
            request_type,
            response_type,
            cardinality,
            request_descriptor,
        }
    }

    #[must_use]
    pub const fn path(self) -> &'static str {
        self.path
    }

    #[must_use]
    pub const fn request_type(self) -> &'static str {
        self.request_type
    }

    #[must_use]
    pub const fn response_type(self) -> &'static str {
        self.response_type
    }

    #[must_use]
    pub const fn cardinality(self) -> Cardinality {
        self.cardinality
    }

    #[doc(hidden)]
    #[must_use]
    pub fn request_descriptor(self) -> prost_reflect::MessageDescriptor {
        (self.request_descriptor)()
    }
}

/// An immutable generated service catalog.
#[derive(Clone, Copy, Debug)]
pub struct ServiceDescriptor {
    name: &'static str,
    methods: &'static [Method],
    descriptor_set: &'static [u8],
}

impl ServiceDescriptor {
    #[must_use]
    pub const fn new(
        name: &'static str,
        methods: &'static [Method],
        descriptor_set: &'static [u8],
    ) -> Self {
        Self {
            name,
            methods,
            descriptor_set,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    #[must_use]
    pub const fn methods(self) -> &'static [Method] {
        self.methods
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn descriptor_set(self) -> &'static [u8] {
        self.descriptor_set
    }
}

#[derive(Clone, Default)]
pub(crate) struct Registry {
    methods: BTreeMap<&'static str, RegisteredMethod>,
    services: BTreeSet<&'static str>,
}

impl Registry {
    pub(crate) fn method(&self, path: &str) -> Option<Method> {
        self.methods.get(path).map(|entry| entry.method)
    }

    pub(crate) fn validation(&self, path: &str) -> Option<std::sync::Arc<Validation>> {
        self.methods
            .get(path)
            .map(|entry| std::sync::Arc::clone(&entry.validation))
    }

    pub(crate) fn services(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.services.iter().copied()
    }
}

#[derive(Clone)]
struct RegisteredMethod {
    method: Method,
    validation: std::sync::Arc<Validation>,
}

/// The only application-facing registration boundary.
#[derive(Clone, Default)]
pub struct Services {
    routes: Routes,
    registry: Registry,
}

impl std::fmt::Debug for Services {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Services").finish_non_exhaustive()
    }
}

impl Services {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one generated, governed service exactly once.
    ///
    /// Generated code calls this after applying the fixed four MiB codec
    /// bounds.  A hand-written service cannot add a peer-selected method or
    /// replace an already registered descriptor.
    #[doc(hidden)]
    pub fn register<S>(&mut self, service: S, descriptor: ServiceDescriptor) -> Result<(), Error>
    where
        S: Service<Request<Body>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
        S::Response: axum::response::IntoResponse,
        S::Future: Send + 'static,
    {
        if descriptor.name != S::NAME
            || descriptor.methods.is_empty()
            || self.registry.services.contains(descriptor.name)
            || !descriptor_matches_fds(descriptor)
        {
            return Err(Error::InvalidRegistration);
        }

        let mut additions = Vec::with_capacity(descriptor.methods.len());
        let mut paths = BTreeSet::new();
        let validation = Validation::compile(descriptor)?;
        for method in descriptor.methods {
            let expected_prefix = format!("/{}/", descriptor.name);
            if !method.path.starts_with(&expected_prefix)
                || method.path.len() == expected_prefix.len()
                || method.request_type.is_empty()
                || method.response_type.is_empty()
                || method.request_descriptor().full_name() != method.request_type
                || !paths.insert(method.path)
                || self.registry.methods.contains_key(method.path)
            {
                return Err(Error::InvalidRegistration);
            }
            additions.push((
                method.path,
                RegisteredMethod {
                    method: *method,
                    validation: std::sync::Arc::clone(&validation),
                },
            ));
        }

        self.registry.services.insert(descriptor.name);
        self.registry.methods.extend(additions);
        self.routes = std::mem::take(&mut self.routes).add_service(service);
        Ok(())
    }

    pub(crate) fn into_parts(self) -> (Routes, Registry) {
        (self.routes, self.registry)
    }
}

pub(crate) fn descriptor_matches_fds(descriptor: ServiceDescriptor) -> bool {
    let Ok(pool) = prost_reflect::DescriptorPool::decode(descriptor.descriptor_set()) else {
        return false;
    };
    let expected = descriptor
        .methods()
        .iter()
        .map(|method| (method.path(), method))
        .collect::<BTreeMap<_, _>>();
    let Some(service) = pool.get_service_by_name(descriptor.name()) else {
        return false;
    };
    if service.methods().len() != expected.len() {
        return false;
    }
    service.methods().all(|method| {
        let path = format!("/{}/{}", descriptor.name(), method.name());
        let Some(expected) = expected.get(path.as_str()) else {
            return false;
        };
        let cardinality = match (method.is_client_streaming(), method.is_server_streaming()) {
            (false, false) => Cardinality::Unary,
            (true, false) => Cardinality::ClientStreaming,
            (false, true) => Cardinality::ServerStreaming,
            (true, true) => Cardinality::BidiStreaming,
        };
        method.input().full_name() == expected.request_type()
            && method.output().full_name() == expected.response_type()
            && cardinality == expected.cardinality()
    })
}
