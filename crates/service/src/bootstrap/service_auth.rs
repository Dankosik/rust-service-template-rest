//! Service token verification and signing from the `service_auth` section.

use infra_serviceauth::{
    CallerName, JwtType, SigningError, SigningOptions, TokenSource, TrustRegistry, Verifier,
};
use service_config::{Config, ServiceAuthConfig, ValidationError};

/// What the gRPC listener verifies and what outbound clients sign with.
pub(super) struct PreparedServiceAuth {
    /// `Some` exactly when `service_auth.mode = "jwt"`.
    pub(super) verifier: Option<Verifier>,
    pub(super) tokens: TokenSource,
}

/// Admits the trust inputs before any listener binds. The adapter owns the
/// name, type, registry and key grammar; each refusal names its key.
pub(super) fn prepare(config: &Config) -> Result<PreparedServiceAuth, ValidationError> {
    let ServiceAuthConfig::Jwt {
        name,
        jwt_type,
        trust,
        key_id,
        private_key,
    } = &config.service_auth
    else {
        return Ok(PreparedServiceAuth {
            verifier: None,
            tokens: TokenSource::disabled(),
        });
    };
    let invalid =
        |key: &str, error: &dyn std::fmt::Display| ValidationError::new(key, error.to_string());
    let name = CallerName::parse(name).map_err(|error| invalid("service_auth.name", &error))?;
    let jwt_type =
        JwtType::parse(jwt_type).map_err(|error| invalid("service_auth.jwt_type", &error))?;
    let trust =
        TrustRegistry::parse(trust).map_err(|error| invalid("service_auth.trust", &error))?;
    let tokens = match (key_id, private_key) {
        (Some(key_id), Some(private_key)) => TokenSource::new(SigningOptions {
            name: name.clone(),
            jwt_type: jwt_type.clone(),
            key_id: key_id.clone(),
            private_key: private_key.clone(),
        })
        .map_err(|error| {
            let key = match error {
                SigningError::InvalidKeyId => "service_auth.key_id",
                _ => "service_auth.private_key",
            };
            invalid(key, &error)
        })?,
        _ => TokenSource::without_key(),
    };
    tracing::info!(
        service_auth.name = %name,
        service_auth.trusted_callers = trust.len(),
        service_auth.signing = key_id.is_some(),
        "service_auth_prepared"
    );
    Ok(PreparedServiceAuth {
        verifier: Some(Verifier::new(name, jwt_type, trust)),
        tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra_serviceauth::test_support::{TestKey, registry};

    fn jwt(trust: String, signing: Option<(&str, String)>) -> Config {
        let (key_id, private_key) = signing.map_or((None, None), |(key_id, pem)| {
            (Some(key_id.to_owned()), Some(pem.into()))
        });
        Config {
            service_auth: ServiceAuthConfig::Jwt {
                name: "catalog".to_owned(),
                jwt_type: "example-svc+jwt".to_owned(),
                trust,
                key_id,
                private_key,
            },
            ..Config::default()
        }
    }

    #[test]
    fn jwt_mode_verifies_and_signs_only_with_admitted_inputs() {
        let key = TestKey::new("catalog/1");
        let trust = registry(&[("consultant", &[&key])]);
        let prepared = prepare(&jwt(trust.clone(), Some(("catalog/1", key.pem())))).unwrap();
        assert_eq!(
            prepared.verifier.as_ref().map(Verifier::trusted_callers),
            Some(1)
        );
        assert!(
            prepared
                .tokens
                .receiver("consultant")
                .unwrap()
                .authorization()
                .unwrap()
                .is_some()
        );
        let receiving_only = prepare(&jwt(trust.clone(), None)).unwrap();
        assert!(receiving_only.verifier.is_some());
        assert!(receiving_only.tokens.receiver("consultant").is_err());
        let off = prepare(&Config::default()).unwrap();
        assert!(off.verifier.is_none());
        assert_eq!(
            off.tokens.receiver("consultant").unwrap().authorization(),
            Ok(None)
        );
        for (config, key) in [
            (jwt("{}".to_owned(), None), "service_auth.trust"),
            (
                jwt(trust.clone(), Some(("two words", key.pem()))),
                "service_auth.key_id",
            ),
            (
                jwt(trust, Some(("catalog/1", "not a key".to_owned()))),
                "service_auth.private_key",
            ),
        ] {
            assert_eq!(
                prepare(&config).err().map(|error| error.key),
                Some(key.to_owned())
            );
        }
    }
}
