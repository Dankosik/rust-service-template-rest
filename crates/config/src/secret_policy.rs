//! Which keys may only be set through the environment.
//!
//! Baseline files describe non-secret defaults. A secret-like key may appear
//! in a file only as an empty placeholder that documents the key; a non-empty
//! value there is a leaked credential waiting to be committed.

use secrecy::SecretString;
use serde::Deserialize;

/// Missing, empty, or whitespace-only secret is absent (`None`).
///
/// Trim decides vacancy only; a present secret keeps the deserialized
/// bytes, including padding. Downstream parsers (DSN, OTLP headers) trim
/// their own input.
pub(crate) fn occupied_secret<'de, D>(deserializer: D) -> Result<Option<SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|s| (!s.trim().is_empty()).then(|| SecretString::from(s))))
}

/// Whether a dotted key names a credential.
///
/// Segments are split on `.`, `_`, and `-`, so `otlp_headers`, `api_key`,
/// and `private-key` all match; `token_profile` and `token_url` do not.
#[must_use]
pub fn is_secret_like_key(key: &str) -> bool {
    let lowered = key.trim().to_ascii_lowercase();
    let segments: Vec<&str> = lowered
        .split(['.', '_', '-'])
        .filter(|segment| !segment.is_empty())
        .collect();
    segments
        .iter()
        .enumerate()
        .any(|(i, segment)| match *segment {
            "password" | "secret" | "secrets" | "authorization" | "dsn" => true,
            "token" => !matches!(segments.get(i + 1), Some(&"profile" | &"url")),
            "key" => matches!(segments.get(i.wrapping_sub(1)), Some(&"api" | &"private")),
            "headers" => matches!(segments.get(i.wrapping_sub(1)), Some(&"otlp")),
            _ => false,
        })
}

/// Return the first secret-like key in `table` that carries a non-empty
/// scalar value, as a dotted path.
pub(crate) fn first_secret_like_key(table: &toml::Table) -> Option<String> {
    let mut path = Vec::new();
    find_secret(table, &mut path)
}

fn find_secret(table: &toml::Table, path: &mut Vec<String>) -> Option<String> {
    for (key, value) in table {
        path.push(key.clone());
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
            "postgres.dsn",
            "authn.introspection_client_secret",
            "observability.otel.exporter.otlp_headers",
            "webhooks.static_secrets",
            "outbound.api_key",
            "signing.private-key",
            "auth.token",
            "http.authorization",
        ] {
            assert!(is_secret_like_key(key), "{key} should be secret-like");
        }
        for key in [
            "authn.token_profile",
            "authn.token_url",
            "http.addr",
            "observability.otel.exporter.otlp_endpoint",
            "cache.key_prefix",
            "keyboard.layout",
        ] {
            assert!(!is_secret_like_key(key), "{key} should not be secret-like");
        }
    }

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

    #[test]
    fn empty_placeholders_are_allowed() {
        let table: toml::Table =
            toml::from_str("[observability.otel.exporter]\notlp_headers = \"\"\n").unwrap();
        assert_eq!(first_secret_like_key(&table), None);
    }
}
