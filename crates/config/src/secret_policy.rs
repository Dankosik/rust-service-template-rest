const SECRET_KEY_PATTERNS: &[&str] = &[
    "observability.otel.exporter.otlp_headers",
    // template:begin grpc:secret-policy-grpc
    "grpc.private_key",
    // template:end grpc:secret-policy-grpc
    // template:begin oidc-introspection:secret-policy-introspection
    "authn.introspection_client_secret",
    // template:end oidc-introspection:secret-policy-introspection
    // template:begin service-auth:secret-policy-service-auth
    "service_auth.private_key",
    // template:end service-auth:secret-policy-service-auth
    // template:begin postgres:secret-policy-postgres
    "postgres.dsn",
    "postgres.migration_dsn",
    // template:end postgres:secret-policy-postgres
    // template:begin cache:secret-policy-cache
    "cache.dsn",
    // template:end cache:secret-policy-cache
    // template:begin messaging:secret-policy-messaging
    "messaging.credentials",
    // template:end messaging:secret-policy-messaging
    // template:begin object-storage:secret-policy-object-storage
    "object_storage.secret_access_key",
    // template:end object-storage:secret-policy-object-storage
    // template:begin clickhouse:secret-policy-clickhouse
    "clickhouse.password",
    // template:end clickhouse:secret-policy-clickhouse
    // template:begin inbound-webhooks:secret-policy-inbound-webhooks
    "inbound_webhooks.secrets.*",
    // template:end inbound-webhooks:secret-policy-inbound-webhooks
    // template:begin webhooks:secret-policy-webhooks
    "webhooks.endpoints.*.secret",
    "webhooks.endpoints.*.previous_secret",
    // template:end webhooks:secret-policy-webhooks
];

#[must_use]
pub(crate) fn is_secret_key(key: &str) -> bool {
    let trimmed = key.trim();
    SECRET_KEY_PATTERNS
        .iter()
        .any(|pattern| matches_pattern(pattern, trimmed))
}

fn matches_pattern(pattern: &str, key: &str) -> bool {
    let mut pattern_segments = pattern.split('.');
    let mut key_segments = key.split('.');
    loop {
        match (pattern_segments.next(), key_segments.next()) {
            (Some("*"), Some(segment)) if !segment.is_empty() => {}
            (Some(expected), Some(segment)) if segment.eq_ignore_ascii_case(expected) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

pub(crate) fn first_secret_key(table: &toml::Table) -> Option<String> {
    let mut segments = Vec::new();
    find_secret(table, &mut segments)
}

fn find_secret<'a>(table: &'a toml::Table, segments: &mut Vec<&'a str>) -> Option<String> {
    for (key, value) in table {
        segments.push(key);
        let dotted = segments.join(".");
        let found = if is_secret_key(&dotted) && has_value(value) {
            Some(dotted)
        } else {
            value
                .as_table()
                .and_then(|nested| find_secret(nested, segments))
        };
        segments.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn has_value(value: &toml::Value) -> bool {
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
    fn classifies_secret_keys() {
        for key in [
            "observability.otel.exporter.otlp_headers",
            "  OBSERVABILITY.OTEL.EXPORTER.OTLP_HEADERS  ",
            // template:begin grpc:secret-key-grpc-vector
            "grpc.private_key",
            // template:end grpc:secret-key-grpc-vector
            // template:begin oidc-introspection:secret-key-introspection-vector
            "authn.introspection_client_secret",
            // template:end oidc-introspection:secret-key-introspection-vector
            // template:begin service-auth:secret-key-service-auth-vector
            "service_auth.private_key",
            // template:end service-auth:secret-key-service-auth-vector
            // template:begin postgres:secret-key-vector
            "postgres.dsn",
            "postgres.migration_dsn",
            // template:end postgres:secret-key-vector
            // template:begin cache:secret-key-cache-vector
            "cache.dsn",
            // template:end cache:secret-key-cache-vector
            // template:begin messaging:secret-key-messaging-vector
            "messaging.credentials",
            // template:end messaging:secret-key-messaging-vector
            // template:begin object-storage:secret-key-object-storage-vector
            "object_storage.secret_access_key",
            // template:end object-storage:secret-key-object-storage-vector
            // template:begin clickhouse:secret-key-clickhouse-vector
            "clickhouse.password",
            // template:end clickhouse:secret-key-clickhouse-vector
            // template:begin inbound-webhooks:secret-key-inbound-webhooks-vector
            "inbound_webhooks.secrets.partner_v2",
            // template:end inbound-webhooks:secret-key-inbound-webhooks-vector
            // template:begin webhooks:secret-key-webhooks-vector
            "webhooks.endpoints.partner.secret",
            "webhooks.endpoints.partner.previous_secret",
            // template:end webhooks:secret-key-webhooks-vector
        ] {
            assert!(is_secret_key(key), "{key} should be secret");
        }
        for key in [
            "cors.allow_credentials",
            "auth.token_ttl",
            "rate_limit.token_bucket",
            "authn.token_profile",
            "object_storage.credentials",
            "postgres.password_file",
            "messaging.credentials_file",
            "service_auth.key_id",
            "service_auth.jwt_type",
            "service_auth.trust",
            "http.addr",
            "observability.otel.exporter.otlp_endpoint",
            "cache.key_prefix",
            "cache.client_key_path",
            "clickhouse.password_file",
            "clickhouse.client_key_path",
            "clickhouse.root_ca_path",
            "object_storage.access_key_id",
            "inbound_webhooks.secrets.",
            "inbound_webhooks.secrets.partner.extra",
            "webhooks.endpoints.partner.url",
            "webhooks.endpoints..secret",
        ] {
            assert!(!is_secret_key(key), "{key} should not be secret");
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
        assert_eq!(first_secret_key(&table).as_deref(), Some("postgres.dsn"));
    }
    // template:end postgres:secret-policy-postgres-fixture
}
