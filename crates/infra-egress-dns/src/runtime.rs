use std::{fmt, io, sync::Arc};

use hickory_resolver::{
    TokioResolver,
    config::{LookupIpStrategy, ResolveHosts},
};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::{ResolveError, admit_answers};

/// Reqwest DNS resolver with cached whole-answer public-address admission.
#[derive(Clone)]
pub struct PublicAddressResolver {
    resolver: Arc<TokioResolver>,
}

impl fmt::Debug for PublicAddressResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicAddressResolver([LIBRARY_OWNED])")
    }
}

impl PublicAddressResolver {
    /// Snapshots system resolver configuration without performing a lookup.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::Configuration`] when system DNS settings cannot
    /// be read or cannot produce a supported resolver.
    pub fn new() -> Result<Self, ResolveError> {
        let mut builder = TokioResolver::builder_tokio()
            .map_err(|source| ResolveError::Configuration(Box::new(source)))?;
        let options = builder.options_mut();
        if options.timeout.is_zero()
            || std::time::Instant::now()
                .checked_add(options.timeout)
                .is_none()
            || options.attempts == 0
        {
            return Err(ResolveError::Configuration(Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                "system DNS timeout and attempts must be positive and representable",
            ))));
        }
        options.use_hosts_file = ResolveHosts::Never;
        options.ip_strategy = LookupIpStrategy::Ipv6AndIpv4;
        options.cache_size = 8_192;
        options.num_concurrent_reqs = 2;
        options.max_active_requests = 32;
        options.allow_answers.clear();
        options.deny_answers.clear();

        builder
            .build()
            .map(|resolver| Self {
                resolver: Arc::new(resolver),
            })
            .map_err(|source| ResolveError::Configuration(Box::new(source)))
    }
}

impl Resolve for PublicAddressResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.resolver.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let lookup = resolver.lookup_ip(host).await.map_err(|source| {
                Box::new(ResolveError::Lookup(Box::new(source)))
                    as Box<dyn std::error::Error + Send + Sync>
            })?;
            let addresses = lookup.iter().collect();
            let admitted = admit_answers(addresses)
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(admitted.into_iter()) as Addrs)
        })
    }
}
