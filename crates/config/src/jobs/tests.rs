use std::num::NonZeroU32;

use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from, load_jobs_operator_from};
use crate::test_support::{BUILD, env, write};

fn parse(toml: &str) -> Result<JobsConfig, toml::de::Error> {
    toml::from_str(toml)
}

fn workers(max_workers: u32) -> JobsConfig {
    JobsConfig {
        max_workers: NonZeroU32::new(max_workers).unwrap(),
    }
}

fn postgres(max_connections: u32) -> PostgresConfig {
    PostgresConfig {
        max_connections: NonZeroU32::new(max_connections).unwrap(),
        ..PostgresConfig::default()
    }
}

#[test]
fn default_is_one_and_validates() {
    let config = JobsConfig::default();
    assert_eq!(config.max_workers, NonZeroU32::MIN);
    config.validate().unwrap();
}

#[rstest::rstest]
#[case::just_above_maximum(501, false)]
#[case::minimum(1, true)]
#[case::maximum(500, true)]
fn validate_bounds(#[case] max_workers: u32, #[case] accepted: bool) {
    let config = workers(max_workers);
    match config.validate() {
        Ok(()) => assert!(accepted),
        Err(err) => {
            assert!(!accepted);
            assert_eq!(err.key, "jobs.max_workers");
        }
    }
}

#[test]
fn zero_max_workers_fails_to_deserialize() {
    let err = parse("max_workers = 0").unwrap_err();
    assert!(err.to_string().contains("max_workers"), "{err}");
}

#[test]
fn toml_sets_max_workers() {
    let config = parse("max_workers = 8").unwrap();
    assert_eq!(config.max_workers.get(), 8);
}

#[test]
fn unknown_keys_in_the_section_fail() {
    assert!(parse("max_workers = 8\nbogus = 1").is_err());
}

#[test]
fn one_worker_needs_three_connections() {
    let err = workers(1).validate_pool_capacity(&postgres(2)).unwrap_err();
    assert_eq!(err.key, "postgres.max_connections");
    assert_eq!(
        err.message,
        "must be at least jobs.max_workers + 2 (3) for the jobs worker, got 2"
    );
    assert_eq!(
        err.to_string(),
        "postgres.max_connections: must be at least jobs.max_workers + 2 (3) for the jobs worker, got 2"
    );
}

#[test]
fn one_worker_accepts_three_connections() {
    workers(1).validate_pool_capacity(&postgres(3)).unwrap();
}

#[test]
fn eight_workers_need_ten_connections() {
    let jobs = workers(8);
    let err = jobs.validate_pool_capacity(&postgres(9)).unwrap_err();
    assert_eq!(err.key, "postgres.max_connections");
    assert!(err.message.contains("(10)"), "{err}");
    jobs.validate_pool_capacity(&postgres(10)).unwrap();
}

#[test]
fn defaults_satisfy_the_pool_bound() {
    JobsConfig::default()
        .validate_pool_capacity(&PostgresConfig::default())
        .unwrap();
}

#[test]
fn five_hundred_workers_refuse_five_hundred_connections() {
    let err = workers(500)
        .validate_pool_capacity(&postgres(500))
        .unwrap_err();
    assert_eq!(err.key, "postgres.max_connections");
}

// template:begin outbox:jobs-outbox-capacity-tests
#[rstest::rstest]
#[case::outbox_only(500, false, 3, "must be at least 3 (3) for the outbox worker, got 2")]
#[case::combined_one(
    1,
    true,
    6,
    "must be at least jobs.max_workers + 5 (6) for the outbox worker, got 5"
)]
#[case::combined_eight(
    8,
    true,
    13,
    "must be at least jobs.max_workers + 5 (13) for the outbox worker, got 12"
)]
fn publisher_capacity_is_reserved_from_ordinary_slots(
    #[case] max_workers: u32,
    #[case] ordinary_jobs: bool,
    #[case] required: u32,
    #[case] message: &str,
) {
    let jobs = workers(max_workers);
    let error = jobs
        .validate_pool_capacity_with_outbox(&postgres(required - 1), ordinary_jobs)
        .unwrap_err();
    assert_eq!(error.key, "postgres.max_connections");
    assert_eq!(error.message, message);
    jobs.validate_pool_capacity_with_outbox(&postgres(required), ordinary_jobs)
        .unwrap();
}
// template:end outbox:jobs-outbox-capacity-tests

#[test]
fn jobs_max_workers_is_not_secret_like() {
    assert!(!crate::secret_policy::is_secret_like_key(
        "jobs.max_workers"
    ));
}

#[test]
fn jobs_environment_sets_max_workers() {
    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__JOBS__MAX_WORKERS", "8")]),
    )
    .unwrap();
    assert_eq!(cfg.jobs.max_workers.get(), 8);
}

#[test]
fn jobs_operator_ignores_unrelated_sections_and_worker_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "operator.toml",
        "[postgres]\nenabled = true\nmax_connections = 1\n\
         [http]\nrequest_timeout = \"invalid\"\n\
         [jobs]\nmax_workers = 0\n",
    );
    let cfg = load_jobs_operator_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        env(&[
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
            ("APP__AUTHN__MODE", "invalid"),
            ("APP__MESSAGING__ENABLED", "invalid"),
            ("APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES", "invalid"),
            ("APP__OBJECT_STORAGE__ENABLED", "invalid"),
            ("APP__LOG__FORMAT", "invalid"),
            ("APP__OBSERVABILITY__OTEL", "invalid"),
        ]),
    )
    .unwrap();
    assert!(cfg.postgres.enabled);
    assert!(cfg.postgres.dsn.is_some());
    assert_eq!(cfg.postgres.max_connections.get(), 1);
}

#[test]
fn jobs_operator_uses_the_shared_source_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = tempfile::tempdir().unwrap();
    let mut options = LoadOptions::default();
    let defaults = load_jobs_operator_from(&options, env(&[])).unwrap();
    assert_eq!(defaults.postgres.max_connections.get(), 4);

    options.config = Some(write(
        &dir,
        "base.toml",
        "[postgres]\nmax_connections = 5\n",
    ));
    let base = load_jobs_operator_from(&options, env(&[])).unwrap();
    assert_eq!(base.postgres.max_connections.get(), 5);
    for connections in [6, 7] {
        options.config_overlay.push(write(
            &dir,
            &format!("overlay-{connections}.toml"),
            &format!("[postgres]\nmax_connections = {connections}\n"),
        ));
        let overlay = load_jobs_operator_from(&options, env(&[])).unwrap();
        assert_eq!(overlay.postgres.max_connections.get(), connections);
    }
    write(&secrets, "APP__POSTGRES__MAX_CONNECTIONS", "8");
    write(&secrets, "APP__POSTGRES__DSN", "postgres://localhost/app");
    options.secrets_dir = Some(secrets.path().to_owned());
    let secret = load_jobs_operator_from(&options, env(&[])).unwrap();
    assert_eq!(secret.postgres.max_connections.get(), 8);
    assert!(secret.postgres.dsn.is_some());
    let environment = load_jobs_operator_from(
        &options,
        env(&[
            ("APP__POSTGRES__MAX_CONNECTIONS", "9"),
            ("APP__POSTGRES__DSN", ""),
        ]),
    )
    .unwrap();
    assert_eq!(environment.postgres.max_connections.get(), 9);
    assert!(environment.postgres.dsn.is_none());
}

#[test]
fn jobs_operator_keeps_postgres_validation_and_value_free_errors() {
    let options = LoadOptions::default();
    let unknown =
        load_jobs_operator_from(&options, env(&[("APP__POSTGRES__BOGUS", "1")])).unwrap_err();
    assert!(matches!(unknown, Error::Deserialize(_)), "{unknown}");
    for (name, value, key) in [
        (
            "APP__POSTGRES__MAX_CONNECTIONS",
            "501",
            "postgres.max_connections",
        ),
        ("APP__POSTGRES__ENABLED", "true", "postgres.dsn"),
    ] {
        let error = load_jobs_operator_from(&options, env(&[(name, value)])).unwrap_err();
        assert!(
            matches!(&error, Error::Validate(error) if error.key == key),
            "{error}"
        );
    }
    let secret = "fixture-private-value";
    let error =
        load_jobs_operator_from(&options, env(&[("APP__POSTGRES__ENABLED", secret)])).unwrap_err();
    assert!(matches!(error, Error::Deserialize(_)), "{error}");
    assert!(!error.to_string().contains(secret));
    assert!(!format!("{error:?}").contains(secret));
}

#[test]
fn jobs_operator_prescans_unrelated_sections() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[unrelated]\nsecret = \"fixture-private-value\"\n",
    );
    let error = load_jobs_operator_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        env(&[]),
    )
    .unwrap_err();
    assert!(matches!(error, Error::SecretInFile { .. }), "{error}");
    assert!(!error.to_string().contains("fixture-private-value"));

    let unaddressable = write(&dir, "unaddressable.toml", "[Unrelated]\nvalue = 1\n");
    let error = load_jobs_operator_from(
        &LoadOptions {
            config: Some(unaddressable),
            ..LoadOptions::default()
        },
        env(&[]),
    )
    .unwrap_err();
    assert!(matches!(error, Error::UnaddressableKey { .. }), "{error}");
    let malformed = load_jobs_operator_from(
        &LoadOptions::default(),
        env(&[("APP__UNRELATED__", "fixture-private-value")]),
    )
    .unwrap_err();
    assert!(
        matches!(malformed, Error::MalformedEnvName { .. }),
        "{malformed}"
    );
}
