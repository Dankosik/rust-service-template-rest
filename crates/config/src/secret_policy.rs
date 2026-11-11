//! Which keys may only be set through an `APP__` variable.
//!
//! Baseline files describe non-secret defaults. A secret-like key may appear
//! in a file only as an empty placeholder that documents the key; a non-empty
//! value there is a leaked credential waiting to be committed.

/// Whether a dotted key names a credential.
///
/// Segments are split on `.`, `_`, and `-`, so `otlp_headers`, `api_key`,
/// and `private-key` all match; `token_url` remains a non-credential URL name
/// and a key that ends in `password_file` or `credentials_file` a
/// non-credential path. Operator-chosen map keys are segments too, so an
/// integration named `token-exchange` makes its `token_url` secret-like; the
/// guard fails closed rather than learn which segments are names.
#[must_use]
pub(crate) fn is_secret_like_key(key: &str) -> bool {
    let lowered = key.trim().to_ascii_lowercase();
    // template:begin oidc-jwt:secret-policy-token-profile-exception
    // It selects a token contract, not a credential; only this exact key is
    // exempt, so `cache.token_profile` stays secret-like.
    if lowered == "authn.token_profile" {
        return false;
    }
    // template:end oidc-jwt:secret-policy-token-profile-exception
    let segments: Vec<&str> = lowered
        .split(['.', '_', '-'])
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.iter().enumerate().any(|(i, segment)| {
        let previous = i.checked_sub(1).and_then(|j| segments.get(j)).copied();
        let rest = &segments[i + 1..];
        let next = rest.first().copied();
        match *segment {
            "secret" | "secrets" | "authorization" | "dsn" => true,
            "password" | "credentials" => rest != ["file"],
            "token" => next != Some("url"),
            "key" => matches!(previous, Some("api" | "private")),
            "headers" => previous == Some("otlp"),
            _ => false,
        }
    })
}

/// Return the first secret-like key in `table` that carries a non-empty
/// scalar value, as a dotted path.
pub(crate) fn first_secret_like_key(table: &toml::Table) -> Option<String> {
    let mut path = Vec::new();
    find_secret(table, &mut path)
}

fn find_secret<'a>(table: &'a toml::Table, path: &mut Vec<&'a str>) -> Option<String> {
    for (key, value) in table {
        path.push(key);
        let found = match value {
            toml::Value::Table(nested) => find_secret(nested, path),
            scalar => {
                let dotted = path.join(".");
                (is_secret_like_key(&dotted) && has_non_empty_value(scalar)).then_some(dotted)
            }
        };
        path.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn has_non_empty_value(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(text) => !text.trim().is_empty(),
        toml::Value::Array(items) => !items.is_empty(),
        toml::Value::Table(table) => !table.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_secret_like_keys() {
        for key in [
            // template:begin postgres:secret-key-vector
            "postgres.dsn",
            // template:end postgres:secret-key-vector
            // template:begin oidc-introspection:secret-key-introspection-vector
            "authn.introspection_client_secret",
            // template:end oidc-introspection:secret-key-introspection-vector
            "cache.token_profile",
            "observability.otel.exporter.otlp_headers",
            "webhooks.endpoints.partner.secret",
            "webhooks.endpoints.partner.previous_secret",
            "inbound_webhooks.secrets.partner_v2",
            "outbound.api_key",
            "signing.private-key",
            "auth.token",
            "smtp.password",
            "smtp.password_file_contents",
            "http.authorization",
            "messaging.credentials",
            "messaging.credentials_file_contents",
            "object_storage.secret_access_key",
            // template:begin service-auth:secret-key-service-auth-vector
            "service_auth.private_key",
            // template:end service-auth:secret-key-service-auth-vector
        ] {
            assert!(is_secret_like_key(key), "{key} should be secret-like");
        }
        for key in [
            // template:begin oidc-jwt:secret-policy-token-profile-vector
            "authn.token_profile",
            // template:end oidc-jwt:secret-policy-token-profile-vector
            "authn.token_url",
            "database.password_file",
            "messaging.credentials_file",
            "http.addr",
            "observability.otel.exporter.otlp_endpoint",
            "cache.key_prefix",
            "cache.client_key_path",
            "object_storage.access_key_id",
            "keyboard.layout",
            // template:begin service-auth:secret-policy-service-auth-public-vector
            "service_auth.key_id",
            "service_auth.jwt_type",
            "service_auth.trust",
            // template:end service-auth:secret-policy-service-auth-public-vector
        ] {
            assert!(!is_secret_like_key(key), "{key} should not be secret-like");
        }
    }

    // template:begin postgres:secret-policy-postgres-fixture
    #[test]
    fn finds_first_non_empty_secret_value() {
        let table: toml::Table = toml::from_str(
            r#"
            [http]
            addr = ":8080"
            [observability.otel.exporter]
            otlp_endpoint = "http://collector:4318"
            otlp_headers = ""
            [postgres]
            dsn = "postgres://u:p@h/db"
            "#,
        )
        .unwrap();
        assert_eq!(
            first_secret_like_key(&table).as_deref(),
            Some("postgres.dsn")
        );
    }
    // template:end postgres:secret-policy-postgres-fixture
}
