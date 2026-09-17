//! HTTP listen address resolution.
//!
//! Until the configuration stage lands, the only runtime input is the
//! `APP__HTTP__ADDR` variable, named after the future `http.addr` key so the
//! deployment contract does not change when layered configuration arrives.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub(crate) const LISTEN_ADDR_ENV: &str = "APP__HTTP__ADDR";
const DEFAULT_PORT: u16 = 8080;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ListenAddrError {
    #[error("{LISTEN_ADDR_ENV} is not valid UTF-8")]
    NotUtf8,
    #[error("{LISTEN_ADDR_ENV}={value:?} is not a socket address (expected host:port)")]
    Invalid { value: String },
}

/// Resolve the listen address from the process environment.
pub(crate) fn listen_addr_from_env() -> Result<SocketAddr, ListenAddrError> {
    match std::env::var(LISTEN_ADDR_ENV) {
        Ok(value) => parse_listen_addr(&value),
        Err(std::env::VarError::NotPresent) => Ok(default_listen_addr()),
        Err(std::env::VarError::NotUnicode(_)) => Err(ListenAddrError::NotUtf8),
    }
}

/// `0.0.0.0:8080`: every interface, like the Go template's `:8080`.
/// Network admission belongs to the deployment platform, not this process.
fn default_listen_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT)
}

fn parse_listen_addr(value: &str) -> Result<SocketAddr, ListenAddrError> {
    let trimmed = value.trim();
    // Accept the Go-style port-only form `:8080` for deployment parity.
    let candidate = if let Some(port) = trimmed.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else {
        trimmed.to_owned()
    };
    candidate.parse().map_err(|_| ListenAddrError::Invalid {
        value: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binds_every_interface_on_8080() {
        assert_eq!(default_listen_addr(), "0.0.0.0:8080".parse().unwrap());
    }

    #[test]
    fn parses_host_and_port() {
        assert_eq!(
            parse_listen_addr("127.0.0.1:9000").unwrap(),
            "127.0.0.1:9000".parse().unwrap()
        );
        assert_eq!(
            parse_listen_addr("[::1]:9000").unwrap(),
            "[::1]:9000".parse().unwrap()
        );
    }

    #[test]
    fn accepts_port_only_go_style() {
        assert_eq!(
            parse_listen_addr(":9000").unwrap(),
            "0.0.0.0:9000".parse().unwrap()
        );
    }

    #[test]
    fn rejects_hostnames_and_garbage() {
        for value in [
            "localhost:8080",
            "8080",
            "",
            ":",
            "0.0.0.0:99999",
            "http://x:1",
        ] {
            let err = parse_listen_addr(value).unwrap_err();
            assert!(
                matches!(err, ListenAddrError::Invalid { .. }),
                "{value:?} -> {err}"
            );
        }
    }
}
