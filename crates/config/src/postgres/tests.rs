use super::*;
use crate::load::{Error, load_from, load_migration_from};
use crate::test_support::{BUILD, env, write};
use crate::{LoadOptions, LogFormat};
use std::assert_matches;

#[test]
fn defaults_are_inert() {
    let config = PostgresConfig::default();
    assert!(!config.enabled);
    assert!(config.dsn.is_none());
    assert_eq!(config.max_connections.get(), 4);
    assert_eq!(config.password_file, None);
    assert_eq!(config.session_budgets, PostgresSessionBudgets::Startup);
    assert_eq!(config.migration_deadline, Duration::from_secs(300));
    config.validate().unwrap();
}

#[test]
fn enabled_requires_a_dsn() {
    let config = PostgresConfig {
        enabled: true,
        ..PostgresConfig::default()
    };
    let err = config.validate().unwrap_err();
    assert_eq!(err.key, "postgres.dsn");
    assert!(PostgresConfig::default().required_dsn().is_err());
}

#[test]
fn a_disabled_profile_may_keep_its_dsn_placeholder() {
    let config = PostgresConfig {
        enabled: false,
        dsn: Some(SecretString::from(
            "postgres://u:p@h:5432/d?sslmode=require".to_owned(),
        )),
        ..PostgresConfig::default()
    };
    config.validate().unwrap();
}

#[test]
fn pool_size_is_bounded() {
    let config = PostgresConfig {
        max_connections: NonZeroU32::new(501).unwrap(),
        ..PostgresConfig::default()
    };
    let err = config.validate().unwrap_err();
    assert_eq!(err.key, "postgres.max_connections");
}

#[test]
fn migration_deadline_is_bounded() {
    for deadline in [Duration::from_millis(999), Duration::from_secs(86_401)] {
        let config = PostgresConfig {
            migration_deadline: deadline,
            ..PostgresConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "postgres.migration_deadline");
    }
}

#[test]
fn debug_output_redacts_the_dsn() {
    let config = PostgresConfig {
        dsn: Some(SecretString::from(
            "postgres://u:hunter2@h:5432/d?sslmode=require".to_owned(),
        )),
        ..PostgresConfig::default()
    };
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
fn migration_snapshot_ignores_the_sections_it_does_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "service.toml",
        "[http]\nrequest_timeout = \"not a duration\"\n[postgres]\nenabled = true\nmax_connections = 2\n",
    );
    let options = LoadOptions {
        config: Some(file),
        ..LoadOptions::default()
    };
    let variables = [
        ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        (
            "APP__POSTGRES__PASSWORD_FILE",
            "/run/secrets/postgres-password",
        ),
        ("APP__POSTGRES__SESSION_BUDGETS", "server"),
        ("APP__POSTGRES__MIGRATION_DEADLINE", "2h"),
        ("APP__LOG__FORMAT", "text"),
        ("APP__GRPC__ENABLED", "true"),
    ];
    assert!(load_from(&options, BUILD, env(&variables)).is_err());

    let cfg = load_migration_from(&options, BUILD, env(&variables)).unwrap();
    assert!(cfg.postgres.enabled);
    assert_eq!(cfg.postgres.max_connections.get(), 2);
    assert!(cfg.postgres.dsn.is_some());
    assert_eq!(
        cfg.postgres.password_file.as_deref(),
        Some(std::path::Path::new("/run/secrets/postgres-password"))
    );
    assert_eq!(
        cfg.postgres.session_budgets,
        crate::PostgresSessionBudgets::Server
    );
    assert_eq!(
        cfg.postgres.migration_deadline,
        std::time::Duration::from_hours(2)
    );
    assert_eq!(cfg.log.format, LogFormat::Text);
    assert_eq!(cfg.app.version, "1.2.3");
}

#[test]
fn migration_snapshot_keeps_the_rules_of_its_own_sections() {
    let unknown = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__BOGUS", "1")]),
    )
    .unwrap_err();
    assert_matches!(unknown, Error::Deserialize(_), "{unknown}");

    let unknown_source = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__SESSION_BUDGETS", "pooler")]),
    )
    .unwrap_err();
    assert_matches!(unknown_source, Error::Deserialize(_), "{unknown_source}");

    let unbounded = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__MIGRATION_DEADLINE", "25h")]),
    )
    .unwrap_err();
    assert_matches!(&unbounded, Error::Validate(error) if error.key == "postgres.migration_deadline", "{unbounded}");

    let missing_dsn = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__ENABLED", "true")]),
    )
    .unwrap_err();
    assert_matches!(&missing_dsn, Error::Validate(error) if error.key == "postgres.dsn", "{missing_dsn}");

    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[grpc]\nprivate_key = \"private-key-material\"\n",
    );
    let err = load_migration_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(err, Error::SecretInFile { .. }, "{err}");
}
