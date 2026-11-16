use std::path::Path;

use super::*;
use crate::LoadOptions;
use crate::load::load_from;
use crate::test_support::{BUILD, assert_secret_in_file, assert_unknown_env_key_fails, env, write};
use crate::validate::{ValidationErrors, admit_with};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

fn admit(
    input: CacheInput,
    app_env: &str,
    request_timeout: Duration,
) -> Result<CacheConfig, ValidationErrors> {
    admit_with(|v| input.admit(app_env, Some(request_timeout), v))
}

fn rejected_keys(input: CacheInput, app_env: &str) -> Vec<String> {
    admit(input, app_env, REQUEST_TIMEOUT)
        .unwrap_err()
        .iter()
        .map(|error| error.key.clone())
        .collect()
}

#[test]
fn defaults_are_inert() {
    let config = admit(CacheInput::default(), "production", REQUEST_TIMEOUT).unwrap();
    assert!(config.dsn.is_none());
    assert_eq!(config.password_file, None);
    assert_eq!(config.command_timeout, Duration::from_secs(2));
    assert!(config.client_certificate.is_none());
}

#[test]
fn debug_output_redacts_the_dsn() {
    let config = CacheConfig {
        dsn: Some(SecretString::from(
            "redis://:hunter2@127.0.0.1:6379".to_owned(),
        )),
        ..CacheConfig::default()
    };
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
fn allow_flags_are_refused_outside_local_development() {
    for app_env in ["production", "staging"] {
        let plaintext = CacheInput {
            allow_plaintext: true,
            ..CacheInput::default()
        };
        assert_eq!(rejected_keys(plaintext, app_env), ["cache.allow_plaintext"]);
        let unauthenticated = CacheInput {
            allow_unauthenticated: true,
            ..CacheInput::default()
        };
        assert_eq!(
            rejected_keys(unauthenticated, app_env),
            ["cache.allow_unauthenticated"]
        );
    }
    for app_env in ["local", "development"] {
        let local = CacheInput {
            allow_plaintext: true,
            allow_unauthenticated: true,
            ..CacheInput::default()
        };
        let admitted = admit(local, app_env, REQUEST_TIMEOUT).unwrap();
        assert!(admitted.allow_plaintext && admitted.allow_unauthenticated);
    }
}

#[test]
fn a_client_certificate_and_its_key_are_set_together() {
    let certificate_only = CacheInput {
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        ..CacheInput::default()
    };
    assert_eq!(
        rejected_keys(certificate_only, "production"),
        ["cache.client_key_path"]
    );
    let key_only = CacheInput {
        client_key_path: Some(PathBuf::from("/run/tls/client.key")),
        ..CacheInput::default()
    };
    assert_eq!(
        rejected_keys(key_only, "production"),
        ["cache.client_cert_path"]
    );
    let both = CacheInput {
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        client_key_path: Some(PathBuf::from("/run/tls/client.key")),
        ..CacheInput::default()
    };
    let certificate = admit(both, "production", REQUEST_TIMEOUT)
        .unwrap()
        .client_certificate
        .unwrap();
    assert_eq!(certificate.cert_path, Path::new("/run/tls/client.crt"));
    assert_eq!(certificate.key_path, Path::new("/run/tls/client.key"));
}

#[test]
fn every_violated_rule_is_reported_together() {
    let input = CacheInput {
        allow_plaintext: true,
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        command_timeout: Duration::ZERO,
        ..CacheInput::default()
    };
    assert_eq!(
        rejected_keys(input, "production"),
        [
            "cache.allow_plaintext",
            "cache.client_key_path",
            "cache.command_timeout"
        ]
    );
}

#[test]
fn command_timeout_is_bounded() {
    for command_timeout in [Duration::ZERO, Duration::from_millis(10_001)] {
        let input = CacheInput {
            command_timeout,
            ..CacheInput::default()
        };
        assert_eq!(
            rejected_keys(input, "production"),
            ["cache.command_timeout"]
        );
    }
    let edges = CacheInput {
        command_timeout: Duration::from_secs(10),
        ..CacheInput::default()
    };
    admit(edges, "production", REQUEST_TIMEOUT).unwrap();
}

#[test]
fn an_active_cache_must_leave_half_the_request_budget() {
    let active = || CacheInput {
        dsn: Some(SecretString::from(
            "rediss://:secret@cache.example:6379".to_owned(),
        )),
        command_timeout: Duration::from_millis(100),
        ..CacheInput::default()
    };
    let errors = admit(active(), "production", Duration::from_millis(150)).unwrap_err();
    assert!(errors.names("cache.command_timeout"));

    admit(active(), "production", Duration::from_millis(200)).unwrap();
}

#[test]
fn cache_environment_reads_the_dsn_and_redacts_it() {
    use secrecy::ExposeSecret as _;

    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__CACHE__DSN", "redis://:hunter2@127.0.0.1:6379"),
            ("APP__CACHE__COMMAND_TIMEOUT", "200ms"),
            ("APP__APP__ENV", "local"),
            ("APP__CACHE__ALLOW_PLAINTEXT", "true"),
            ("APP__CACHE__ALLOW_UNAUTHENTICATED", "true"),
        ]),
    )
    .unwrap();
    assert_eq!(
        cfg.cache.dsn.as_ref().unwrap().expose_secret(),
        "redis://:hunter2@127.0.0.1:6379"
    );
    assert_eq!(cfg.cache.command_timeout, Duration::from_millis(200));
    assert!(!format!("{cfg:?}").contains("hunter2"));
}

#[test]
fn cache_empty_root_ca_path_unsets_the_file_value() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "ca.toml",
        "[cache]\nroot_ca_path = \" /etc/cache/root-ca.pem \"\n",
    );
    let options = LoadOptions {
        config: Some(file),
        ..LoadOptions::default()
    };
    let set = load_from(&options, BUILD, env(&[])).unwrap();
    assert_eq!(
        set.cache.root_ca_path.as_deref(),
        Some(Path::new("/etc/cache/root-ca.pem"))
    );
    let unset = load_from(&options, BUILD, env(&[("APP__CACHE__ROOT_CA_PATH", " ")])).unwrap();
    assert_eq!(unset.cache.root_ca_path, None);
}

#[test]
fn cache_dsn_in_a_file_is_refused() {
    assert_secret_in_file(
        "[cache]\ndsn = \"redis://:hunter2@127.0.0.1:6379\"\n",
        "cache.dsn",
    );
}

#[test]
fn cache_password_file_is_a_path_a_file_or_the_environment_may_set() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = write(
        &dir,
        "cache.toml",
        "[cache]\npassword_file = \"/run/secrets/from-file\"\n",
    );
    let options = LoadOptions {
        config: Some(overlay),
        ..LoadOptions::default()
    };
    let cfg = load_from(&options, BUILD, env(&[])).unwrap();
    assert_eq!(
        cfg.cache.password_file.as_deref(),
        Some(std::path::Path::new("/run/secrets/from-file"))
    );

    let cfg = load_from(
        &options,
        BUILD,
        env(&[("APP__CACHE__PASSWORD_FILE", "/run/secrets/cache-password")]),
    )
    .unwrap();
    assert_eq!(
        cfg.cache.password_file.as_deref(),
        Some(std::path::Path::new("/run/secrets/cache-password"))
    );

    let cfg = load_from(&options, BUILD, env(&[("APP__CACHE__PASSWORD_FILE", " ")])).unwrap();
    assert_eq!(cfg.cache.password_file, None);
}

#[test]
fn cache_client_certificate_paths_are_set_from_the_environment_together() {
    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__CACHE__CLIENT_CERT_PATH", "/run/tls/client.crt"),
            ("APP__CACHE__CLIENT_KEY_PATH", "/run/tls/client.key"),
        ]),
    )
    .unwrap();
    let certificate = cfg.cache.client_certificate.as_ref().unwrap();
    assert_eq!(certificate.cert_path, Path::new("/run/tls/client.crt"));
    assert_eq!(certificate.key_path, Path::new("/run/tls/client.key"));

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__CACHE__CLIENT_CERT_PATH", "/run/tls/client.crt"),
            ("APP__CACHE__CLIENT_KEY_PATH", ""),
        ]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("cache.client_key_path"), "{err}");
}

#[test]
fn cache_unknown_environment_key_fails_loading() {
    assert_unknown_env_key_fails("APP__CACHE__UNKNOWN");
}
