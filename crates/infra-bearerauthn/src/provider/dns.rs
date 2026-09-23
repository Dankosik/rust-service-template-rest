//! Auth-private DNS error adaptation and raw-answer test facade.

use std::net::IpAddr;

pub(super) use infra_egress_dns::PublicResolver;

use crate::Failure;

pub(super) fn admit_address(address: IpAddr) -> Result<(), Failure> {
    infra_egress_dns::admit_address(address).map_err(|_| Failure::Unavailable)
}

// template:begin oidc-jwt:authn-dns-raw-answer-resolver
/// Test-only resolver that places raw answers immediately upstream of the
/// production address predicate. It cannot alter the production resolver or
/// admit a private destination.
#[cfg(test)]
#[derive(Clone)]
pub(super) struct RawAnswerResolver {
    host: String,
    answers: Vec<IpAddr>,
}

#[cfg(test)]
impl RawAnswerResolver {
    pub(super) fn new(host: &str, answers: Vec<IpAddr>) -> Self {
        Self {
            host: host.to_owned(),
            answers,
        }
    }
}

#[cfg(test)]
impl reqwest::dns::Resolve for RawAnswerResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = self.host.clone();
        let answers = self.answers.clone();
        Box::pin(async move {
            if !name.as_str().eq_ignore_ascii_case(&host)
                || answers.is_empty()
                || answers
                    .iter()
                    .any(|address| admit_address(*address).is_err())
            {
                return Err(std::io::Error::other("provider DNS address denied").into());
            }
            Ok(Box::new(
                answers
                    .into_iter()
                    .map(|address| std::net::SocketAddr::new(address, 0)),
            ) as reqwest::dns::Addrs)
        })
    }
}
// template:end oidc-jwt:authn-dns-raw-answer-resolver
