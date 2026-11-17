use infra_serviceauth::{
    JwtType, KeyId, ServiceName, SigningOptions, TokenSource, TrustRegistry, Verifier,
};
use service_config::{Config, ServiceAuthConfig, ValidationError};

pub(super) struct PreparedServiceAuth {
    pub(super) verifier: Option<Verifier>,
    pub(super) tokens: TokenSource,
}

pub(super) fn prepare(config: &Config) -> Result<PreparedServiceAuth, ValidationError> {
    let ServiceAuthConfig::Jwt {
        name,
        jwt_type,
        trust,
        signing_key,
    } = &config.service_auth
    else {
        return Ok(PreparedServiceAuth {
            verifier: None,
            tokens: TokenSource::disabled(),
        });
    };
    let invalid =
        |key: &str, error: &dyn std::fmt::Display| ValidationError::new(key, error.to_string());
    let name = ServiceName::parse(name).map_err(|error| invalid("service_auth.name", &error))?;
    let jwt_type =
        JwtType::parse(jwt_type).map_err(|error| invalid("service_auth.jwt_type", &error))?;
    let trust =
        TrustRegistry::parse(trust).map_err(|error| invalid("service_auth.trust", &error))?;
    let tokens = match signing_key {
        Some(key) => TokenSource::try_new(SigningOptions {
            name: name.clone(),
            jwt_type: jwt_type.clone(),
            key_id: KeyId::parse(&key.key_id)
                .map_err(|error| invalid("service_auth.key_id", &error))?,
            private_key: key.private_key.clone(),
        })
        .map_err(|error| invalid("service_auth.private_key", &error))?,
        None => TokenSource::without_key(),
    };
    tracing::info!(
        service_auth.name = %name,
        service_auth.trusted_callers = trust.caller_count(),
        service_auth.signing = signing_key.is_some(),
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
    use crate::bootstrap::test_config;
    use infra_serviceauth::test_support::{TestKey, registry};

    fn jwt(trust: &str, signing: Option<(&str, &str)>) -> Config {
        let mut environment = vec![
            ("APP__SERVICE_AUTH__MODE", "jwt"),
            ("APP__SERVICE_AUTH__NAME", "catalog"),
            ("APP__SERVICE_AUTH__JWT_TYPE", "example-svc+jwt"),
            ("APP__SERVICE_AUTH__TRUST", trust),
        ];
        if let Some((key_id, pem)) = signing {
            environment.extend([
                ("APP__SERVICE_AUTH__KEY_ID", key_id),
                ("APP__SERVICE_AUTH__PRIVATE_KEY", pem),
            ]);
        }
        test_config(&environment)
    }

    #[test]
    fn jwt_mode_verifies_and_signs_only_with_admitted_inputs() {
        let key = TestKey::new("catalog/1");
        let trust = registry(&[("orders", &[&key])]);
        let prepared = prepare(&jwt(&trust, Some(("catalog/1", &key.pem())))).unwrap();
        let issued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let token = key.sign(
            &serde_json::json!({"alg": "ES256", "typ": "example-svc+jwt", "kid": "catalog/1"}),
            &serde_json::json!({
                "iss": "orders", "sub": "orders", "aud": "catalog",
                "iat": issued_at, "exp": issued_at + 60,
            }),
        );
        let authorization = format!("Bearer {token}");
        let caller = prepared
            .verifier
            .as_ref()
            .unwrap()
            .authenticate([authorization.as_bytes()])
            .unwrap();
        assert_eq!(caller.name().as_str(), "orders");
        assert!(
            prepared
                .tokens
                .receiver("orders")
                .unwrap()
                .authorization()
                .unwrap()
                .is_some()
        );
        let receiving_only = prepare(&jwt(&trust, None)).unwrap();
        assert!(receiving_only.verifier.is_some());
        assert!(receiving_only.tokens.receiver("orders").is_err());
        let off = prepare(&test_config(&[])).unwrap();
        assert!(off.verifier.is_none());
        assert_eq!(
            off.tokens.receiver("orders").unwrap().authorization(),
            Ok(None)
        );
        for (config, key) in [
            (jwt("{}", None), "service_auth.trust"),
            (
                jwt(&trust, Some(("two words", &key.pem()))),
                "service_auth.key_id",
            ),
            (
                jwt(&trust, Some(("catalog/1", "not a key"))),
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
