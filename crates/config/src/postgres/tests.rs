use super::*;
use crate::load::{Error, load_from, load_migration_from};
use crate::test_support::{BUILD, env, write};
use crate::validate::admit_with;
use crate::{LoadOptions, LogFormat};
use std::assert_matches;

fn dsn() -> SecretString {
    SecretString::from("postgres://u:p@h:5432/d?sslmode=require".to_owned())
}

#[test]
fn defaults_are_inert() {
    let input = PostgresInput::default();
    assert!(!input.enabled);
    assert!(input.dsn.is_none());
    assert_eq!(input.max_connections.get(), 4);
    assert_eq!(input.password_file, None);
    assert_eq!(input.session_budgets, PostgresSessionBudgets::Startup);
    assert_eq!(input.migration_deadline, Duration::from_mins(5));
    assert!(admit_with(|v| input.admit(v)).unwrap().is_none());
}

#[test]
fn enabled_requires_a_dsn() {
    let input = PostgresInput {
        enabled: true,
        ..PostgresInput::default()
    };
    let errors = admit_with(|v| input.admit(v)).unwrap_err();
    assert!(errors.names("postgres.dsn"));
}

#[test]
fn a_disabled_profile_may_keep_its_dsn_placeholder() {
    let input = PostgresInput {
        enabled: false,
        dsn: Some(dsn()),
        ..PostgresInput::default()
    };
    assert!(admit_with(|v| input.admit(v)).unwrap().is_none());
}

#[test]
fn an_enabled_profile_carries_its_dsn() {
    let input = PostgresInput {
        enabled: true,
        dsn: Some(dsn()),
        ..PostgresInput::default()
    };
    let config = admit_with(|v| input.admit(v)).unwrap().unwrap();
    assert_eq!(config.max_connections.get(), 4);
}

#[rstest::rstest]
#[case::disabled(false)]
#[case::enabled(true)]
fn pool_size_is_bounded(#[case] enabled: bool) {
    let input = PostgresInput {
        enabled,
        dsn: Some(dsn()),
        max_connections: NonZeroU32::new(501).unwrap(),
        ..PostgresInput::default()
    };
    let errors = admit_with(|v| input.admit(v)).unwrap_err();
    assert!(errors.names("postgres.max_connections"));
}

#[test]
fn every_postgres_violation_is_reported_together() {
    let input = PostgresInput {
        enabled: true,
        max_connections: NonZeroU32::new(501).unwrap(),
        ..PostgresInput::default()
    };
    let errors = admit_with(|v| input.admit(v)).unwrap_err();
    assert!(errors.names("postgres.dsn"));
    assert!(errors.names("postgres.max_connections"));
}

#[test]
fn pool_size_rejects_fractional_toml() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "postgres.toml", "[postgres]\nmax_connections = 2.8\n");
    let options = LoadOptions {
        config: Some(file),
        ..LoadOptions::default()
    };
    let err = load_from(&options, BUILD, env(&[])).unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    assert!(
        err.to_string().contains("postgres.max_connections"),
        "{err}"
    );
}

#[test]
fn only_migration_loading_enforces_the_deadline_range() {
    let options = LoadOptions::default();
    for deadline in ["0s", "25h"] {
        let variables = [("APP__POSTGRES__MIGRATION_DEADLINE", deadline)];
        load_from(&options, BUILD, env(&variables)).unwrap();
        let err = load_migration_from(&options, BUILD, env(&variables)).unwrap_err();
        assert_matches!(err, Error::Validate(errors) if errors.names("postgres.migration_deadline"));
    }
}

#[test]
fn debug_output_redacts_the_dsn() {
    let config = PostgresConfig {
        dsn: SecretString::from("postgres://u:hunter2@h:5432/d?sslmode=require".to_owned()),
        password_file: None,
        session_budgets: PostgresSessionBudgets::Startup,
        max_connections: NonZeroU32::MIN,
        migration_deadline: Duration::from_mins(5),
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
        // template:begin grpc:postgres-migration-test-grpc-env
        ("APP__GRPC__ENABLED", "true"),
        // template:end grpc:postgres-migration-test-grpc-env
    ];
    assert!(load_from(&options, BUILD, env(&variables)).is_err());

    let cfg = load_migration_from(&options, BUILD, env(&variables)).unwrap();
    let postgres = cfg.postgres.unwrap();
    assert_eq!(postgres.max_connections.get(), 2);
    assert_eq!(
        postgres.password_file.as_deref(),
        Some(std::path::Path::new("/run/secrets/postgres-password"))
    );
    assert_eq!(
        postgres.session_budgets,
        crate::PostgresSessionBudgets::Server
    );
    assert_eq!(
        postgres.migration_deadline,
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

    let typo_section = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRESS__DSN", "postgres://u:secret-pw@localhost/db")]),
    )
    .unwrap_err();
    assert_matches!(&typo_section, Error::Deserialize(_), "{typo_section}");
    assert!(!typo_section.to_string().contains("secret-pw"));
    assert!(!format!("{typo_section:?}").contains("secret-pw"));

    let unknown_source = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__SESSION_BUDGETS", "pooler")]),
    )
    .unwrap_err();
    assert_matches!(unknown_source, Error::Deserialize(_), "{unknown_source}");

    let missing_dsn = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__ENABLED", "true")]),
    )
    .unwrap_err();
    assert_matches!(&missing_dsn, Error::Validate(errors) if errors.names("postgres.dsn"), "{missing_dsn}");

    let dir = tempfile::tempdir().unwrap();
    let unknown_table = write(&dir, "unknown.toml", "[unrelated]\nvalue = 1\n");
    let unknown_err = load_migration_from(
        &LoadOptions {
            config: Some(unknown_table),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(unknown_err, Error::Deserialize(_), "{unknown_err}");

    let leaked = write(
        &dir,
        "leaked.toml",
        "[observability.otel.exporter]\notlp_headers = \"private-key-material\"\n",
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
