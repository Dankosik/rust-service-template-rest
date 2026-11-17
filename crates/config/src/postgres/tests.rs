use super::*;
use crate::load::{load_from, load_migration_from};
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
    assert_eq!(input.max_connections.0.get(), 4);
    assert_eq!(input.password_file, None);
    assert_eq!(input.session_budgets.0, PostgresSessionBudgets::Startup);
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
        max_connections: PoolCapacity(NonZeroU32::new(501).unwrap()),
        ..PostgresInput::default()
    };
    let errors = admit_with(|v| input.admit(v)).unwrap_err();
    assert!(errors.names("postgres.max_connections"));
}

#[test]
fn every_postgres_violation_is_reported_together() {
    let input = PostgresInput {
        enabled: true,
        max_connections: PoolCapacity(NonZeroU32::new(501).unwrap()),
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
fn only_migration_loading_decodes_and_validates_the_deadline() {
    let options = LoadOptions::default();
    let defaults = load_migration_from(
        &options,
        BUILD,
        env(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        ]),
    )
    .unwrap();
    assert_eq!(
        defaults.postgres().migration_deadline,
        Duration::from_mins(5)
    );
    for deadline in ["0s", "25h", "invalid"] {
        let variables = [("APP__POSTGRES__MIGRATION_DEADLINE", deadline)];
        load_from(&options, BUILD, env(&variables)).unwrap();
        // template:begin jobs:pooled-worker-ignores-migration-deadline
        crate::load::load_worker_from(&options, BUILD, env(&variables)).unwrap();
        // template:end jobs:pooled-worker-ignores-migration-deadline
        let err = load_migration_from(&options, BUILD, env(&variables)).unwrap_err();
        if deadline == "invalid" {
            assert_matches!(err, MigrationLoadError::Config(Error::Deserialize(_)));
        } else {
            assert_matches!(
                err,
                MigrationLoadError::Config(Error::Validate(errors))
                    if errors.names("postgres.migration_deadline")
            );
        }
    }
}

#[test]
fn debug_output_redacts_the_dsn() {
    let config = PostgresConfig {
        dsn: SecretString::from("postgres://u:hunter2@h:5432/d?sslmode=require".to_owned()),
        password_file: None,
        session_budgets: PostgresSessionBudgets::Startup,
        max_connections: NonZeroU32::MIN,
    };
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

// template:begin jobs:exclude-messaging-only-postgres-test
#[cfg(any())]
// template:end jobs:exclude-messaging-only-postgres-test
// template:begin worker:messaging-only-postgres-test
#[test]
fn messaging_worker_ignores_the_service_database() {
    let options = LoadOptions::default();
    let variables = [
        ("APP__POSTGRES__ENABLED", "true"),
        ("APP__POSTGRES__MAX_CONNECTIONS", "invalid"),
    ];
    crate::load::load_worker_from(&options, BUILD, env(&variables)).unwrap();
    assert!(load_from(&options, BUILD, env(&variables)).is_err());
    let unknown =
        crate::load::load_worker_from(&options, BUILD, env(&[("APP__DATABASE__ENABLED", "true")]))
            .unwrap_err();
    assert_matches!(unknown, Error::Deserialize(_));
}
// template:end worker:messaging-only-postgres-test

#[test]
fn migration_snapshot_ignores_the_sections_it_does_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "service.toml",
        "[http]\nrequest_timeout = \"not a duration\"\n[postgres]\nenabled = true\nmax_connections = \"bad pool size\"\n",
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
        ("APP__POSTGRES__SESSION_BUDGETS", "invalid"),
        ("APP__POSTGRES__MIGRATION_DEADLINE", "2h"),
        ("APP__LOG__FORMAT", "text"),
        ("APP__OBSERVABILITY__METRICS__ADDR", "invalid-listener"),
        ("APP__OBSERVABILITY__OTEL__SERVICE_NAME", "orders"),
        (
            "APP__OBSERVABILITY__OTEL__TRACES_SAMPLER",
            "invalid-sampler",
        ),
        (
            "APP__OBSERVABILITY__OTEL__TRACES_SAMPLER_ARG",
            "invalid-ratio",
        ),
        (
            "APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT",
            "invalid-endpoint",
        ),
        // template:begin grpc:postgres-migration-test-grpc-env
        ("APP__GRPC__ENABLED", "true"),
        // template:end grpc:postgres-migration-test-grpc-env
    ];
    assert!(load_from(&options, BUILD, env(&variables)).is_err());

    let cfg = load_migration_from(&options, BUILD, env(&variables)).unwrap();
    let postgres = cfg.postgres();
    assert_eq!(
        postgres.password_file.as_deref(),
        Some(std::path::Path::new("/run/secrets/postgres-password"))
    );
    assert_eq!(
        postgres.migration_deadline,
        std::time::Duration::from_hours(2)
    );
    assert_eq!(cfg.log().format, LogFormat::Text);
    assert_eq!(cfg.app().version, "1.2.3");
    assert_eq!(cfg.service_name(), "orders");
}

#[test]
fn migration_uses_the_service_identity_and_rejects_invalid_overrides() {
    let options = LoadOptions::default();
    let migration = load_migration_from(
        &options,
        BUILD,
        env(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        ]),
    )
    .unwrap();
    let service = load_from(&options, BUILD, env(&[])).unwrap();
    assert_eq!(
        migration.service_name(),
        service.observability.otel.service_name
    );

    let blank = load_migration_from(
        &options,
        BUILD,
        env(&[("APP__OBSERVABILITY__OTEL__SERVICE_NAME", "  ")]),
    )
    .unwrap_err();
    assert_matches!(
        blank,
        MigrationLoadError::Config(Error::Validate(errors))
            if errors.names("observability.otel.service_name")
    );

    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "non-string.toml",
        "[observability.otel]\nservice_name = 123\n",
    );
    let non_string = load_migration_from(
        &LoadOptions {
            config: Some(file),
            ..options
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(
        non_string,
        MigrationLoadError::Config(Error::Deserialize(_)),
        "{non_string}"
    );
}

#[test]
fn migration_ignores_only_the_replica_identity_within_app() {
    let dir = tempfile::tempdir().unwrap();
    let enabled = [
        ("APP__POSTGRES__ENABLED", "true"),
        ("APP__POSTGRES__DSN", "postgres://localhost/app"),
    ];
    let options = LoadOptions {
        config: Some(write(
            &dir,
            "replica.toml",
            "[app]\nenv = 'production'\ninstance_id = 27\n",
        )),
        ..LoadOptions::default()
    };
    let migration = load_migration_from(&options, BUILD, env(&enabled)).unwrap();
    assert_eq!(migration.app().env, "production");
    assert!(migration.app().instance_id.is_none());
    let service = load_from(&options, BUILD, env(&enabled)).unwrap_err();
    assert_matches!(service, Error::Deserialize(_), "{service}");

    for key in ["env", "version", "commit", "instance_id_typo"] {
        let options = LoadOptions {
            config: Some(write(
                &dir,
                "invalid-app.toml",
                &format!("[app]\n{key} = 27\n"),
            )),
            ..LoadOptions::default()
        };
        let error = load_migration_from(&options, BUILD, env(&enabled)).unwrap_err();
        assert_matches!(
            error,
            MigrationLoadError::Config(Error::Deserialize(_)),
            "{key}: {error}"
        );
    }
}

#[test]
fn migration_snapshot_keeps_the_rules_of_its_own_sections() {
    for key in [
        "APP__POSTGRES__BOGUS",
        "APP__OBSERVABILITY__EXTRA",
        "APP__OBSERVABILITY__OTEL__EXTRA",
    ] {
        let unknown =
            load_migration_from(&LoadOptions::default(), BUILD, env(&[(key, "1")])).unwrap_err();
        assert_matches!(
            unknown,
            MigrationLoadError::Config(Error::Deserialize(_)),
            "{unknown}"
        );
    }

    let typo_section = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__DATABASE__DSN", "postgres://u:secret-pw@localhost/db")]),
    )
    .unwrap_err();
    assert_matches!(
        &typo_section,
        MigrationLoadError::Config(Error::Deserialize(_)),
        "{typo_section}"
    );
    assert!(!typo_section.to_string().contains("secret-pw"));
    assert!(!format!("{typo_section:?}").contains("secret-pw"));

    let missing_dsn = load_migration_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__POSTGRES__ENABLED", "true")]),
    )
    .unwrap_err();
    assert_matches!(
        &missing_dsn,
        MigrationLoadError::Config(Error::Validate(errors)) if errors.names("postgres.dsn"),
        "{missing_dsn}"
    );

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
    assert_matches!(
        unknown_err,
        MigrationLoadError::Config(Error::Deserialize(_)),
        "{unknown_err}"
    );

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
    assert_matches!(
        err,
        MigrationLoadError::Config(Error::SecretInFile { .. }),
        "{err}"
    );
}
