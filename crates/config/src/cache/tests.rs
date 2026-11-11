use std::path::Path;

use super::*;
use crate::Config;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

#[test]
fn defaults_are_inert() {
    let config = CacheConfig::default();
    assert!(!config.is_active());
    assert_eq!(config.password_file, None);
    assert_eq!(config.command_timeout, Duration::from_secs(2));
    config.validate("production", REQUEST_TIMEOUT).unwrap();
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
        let plaintext = CacheConfig {
            allow_plaintext: true,
            ..CacheConfig::default()
        };
        assert_eq!(
            plaintext
                .validate(app_env, REQUEST_TIMEOUT)
                .unwrap_err()
                .key,
            "cache.allow_plaintext"
        );
        let unauthenticated = CacheConfig {
            allow_unauthenticated: true,
            ..CacheConfig::default()
        };
        assert_eq!(
            unauthenticated
                .validate(app_env, REQUEST_TIMEOUT)
                .unwrap_err()
                .key,
            "cache.allow_unauthenticated"
        );
    }
    let local = CacheConfig {
        allow_plaintext: true,
        allow_unauthenticated: true,
        ..CacheConfig::default()
    };
    local.validate("local", REQUEST_TIMEOUT).unwrap();
    local.validate("development", REQUEST_TIMEOUT).unwrap();
}

#[test]
fn a_client_certificate_and_its_key_are_set_together() {
    let certificate_only = CacheConfig {
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        ..CacheConfig::default()
    };
    assert_eq!(
        certificate_only
            .validate("production", REQUEST_TIMEOUT)
            .unwrap_err()
            .key,
        "cache.client_key_path"
    );
    let key_only = CacheConfig {
        client_key_path: Some(PathBuf::from("/run/tls/client.key")),
        ..CacheConfig::default()
    };
    assert_eq!(
        key_only
            .validate("production", REQUEST_TIMEOUT)
            .unwrap_err()
            .key,
        "cache.client_cert_path"
    );
    let both = CacheConfig {
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        client_key_path: Some(PathBuf::from("/run/tls/client.key")),
        ..CacheConfig::default()
    };
    both.validate("production", REQUEST_TIMEOUT).unwrap();
}

#[test]
fn command_timeout_is_bounded() {
    for command_timeout in [Duration::ZERO, Duration::from_millis(10_001)] {
        let config = CacheConfig {
            command_timeout,
            ..CacheConfig::default()
        };
        assert_eq!(
            config
                .validate("production", REQUEST_TIMEOUT)
                .unwrap_err()
                .key,
            "cache.command_timeout"
        );
    }
    let edges = CacheConfig {
        command_timeout: Duration::from_secs(10),
        ..CacheConfig::default()
    };
    edges.validate("production", REQUEST_TIMEOUT).unwrap();
}

#[test]
fn an_active_cache_must_leave_half_the_request_budget() {
    let mut config = Config::default();
    config.app.version = "0.0.0".to_owned();
    config.app.commit = "test".to_owned();
    config.cache.dsn = Some(SecretString::from(
        "rediss://:secret@cache.example:6379".to_owned(),
    ));
    config.cache.command_timeout = Duration::from_millis(100);
    config.http.request_timeout = Duration::from_millis(150);
    let err = config.validate().unwrap_err();
    assert_eq!(err.key, "cache.command_timeout");

    config.http.request_timeout = Duration::from_millis(200);
    config.validate().unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[cache]\ndsn = \"redis://:hunter2@127.0.0.1:6379\"\n",
    );
    let err = load_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::SecretInFile { key, .. } if key == "cache.dsn"),
        "{err}"
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

    // A blank variable unsets the key, as it does for every optional path.
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
    assert_eq!(
        cfg.cache.client_cert_path.as_deref(),
        Some(std::path::Path::new("/run/tls/client.crt"))
    );
    assert_eq!(
        cfg.cache.client_key_path.as_deref(),
        Some(std::path::Path::new("/run/tls/client.key"))
    );

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
    assert!(matches!(
        load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__CACHE__UNKNOWN", "x")]),
        ),
        Err(Error::Deserialize(_))
    ));
}
