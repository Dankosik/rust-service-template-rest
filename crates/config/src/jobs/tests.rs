use std::assert_matches;

use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_jobs_operator_from, load_worker_from};
use crate::test_support::{BUILD, env, load_worker_env, write};
use crate::validate::validate_with;

fn parse(toml: &str) -> Result<JobsConfig, toml::de::Error> {
    toml::from_str(toml)
}

fn workers(max_workers: u32) -> JobsConfig {
    JobsConfig {
        max_workers: NonZeroU32::new(max_workers).unwrap(),
    }
}

#[test]
fn default_is_one_and_validates() {
    let config = JobsConfig::default();
    assert_eq!(config.max_workers, NonZeroU32::MIN);
    validate_with(|v| config.validate(v)).unwrap();
}

#[rstest::rstest]
#[case::just_above_maximum(501, false)]
#[case::minimum(1, true)]
#[case::maximum(500, true)]
fn validate_bounds(#[case] max_workers: u32, #[case] accepted: bool) {
    let config = workers(max_workers);
    match validate_with(|v| config.validate(v)) {
        Ok(()) => assert!(accepted),
        Err(errors) => {
            assert!(!accepted);
            assert!(errors.names("jobs.max_workers"));
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
fn jobs_max_workers_is_not_secret() {
    assert!(!crate::secret_policy::is_secret_key("jobs.max_workers"));
}

#[test]
fn jobs_environment_sets_max_workers() {
    let cfg = load_worker_env(&[("APP__JOBS__MAX_WORKERS", "8")]);
    assert_eq!(cfg.jobs.max_workers.get(), 8);
}

#[test]
fn worker_config_ignores_service_only_sections_without_demanding_their_secrets() {
    let dir = tempfile::tempdir().unwrap();
    #[allow(
        unused_mut,
        reason = "optional service profiles append their sections when selected"
    )]
    let mut body = String::from("[jobs]\nmax_workers = 2\n");
    // template:begin authn:worker-ignores-service-authn-test
    body.push_str("[authn]\nmode = \"oidc-introspection\"\n");
    // template:end authn:worker-ignores-service-authn-test
    // template:begin grpc:worker-ignores-service-grpc-test
    body.push_str("[grpc]\nenabled = true\nsecurity = \"tls\"\n");
    // template:end grpc:worker-ignores-service-grpc-test
    // template:begin object-storage:worker-ignores-service-object-storage-test
    body.push_str("[object_storage]\nprovider = \"amazon_s3\"\n");
    // template:end object-storage:worker-ignores-service-object-storage-test
    let file = write(&dir, "shared.toml", &body);
    let cfg = load_worker_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    assert_eq!(cfg.jobs.max_workers.get(), 2);

    let unknown = write(&dir, "unknown.toml", "[unrelated]\nvalue = 1\n");
    let err = load_worker_from(
        &LoadOptions {
            config: Some(unknown),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(err, Error::Deserialize(_), "{err}");
}

#[test]
fn jobs_operator_ignores_unrelated_sections_and_worker_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "operator.toml",
        "[postgres]\nenabled = true\nmax_connections = \"bad pool size\"\nmigration_deadline = false\n\
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
            ("APP__POSTGRES__SESSION_BUDGETS", "server"),
            // template:begin authn:jobs-operator-test-authn-env
            ("APP__AUTHN__MODE", "invalid"),
            // template:end authn:jobs-operator-test-authn-env
            // template:begin messaging:jobs-operator-test-messaging-env
            ("APP__MESSAGING__ENABLED", "invalid"),
            // template:end messaging:jobs-operator-test-messaging-env
            // template:begin webhooks:jobs-operator-test-webhooks-env
            ("APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES", "invalid"),
            // template:end webhooks:jobs-operator-test-webhooks-env
            // template:begin object-storage:jobs-operator-test-object-storage-env
            ("APP__OBJECT_STORAGE__ENABLED", "invalid"),
            // template:end object-storage:jobs-operator-test-object-storage-env
            ("APP__LOG__FORMAT", "invalid"),
            ("APP__OBSERVABILITY__OTEL", "invalid"),
        ]),
    )
    .unwrap();
    assert_eq!(
        cfg.postgres().unwrap().session_budgets,
        crate::PostgresSessionBudgets::Server
    );
}

#[test]
fn jobs_operator_uses_the_shared_source_precedence() {
    use std::path::Path;

    fn password_file(config: &crate::JobsOperatorConfig) -> Option<&Path> {
        config.postgres().unwrap().password_file.as_deref()
    }

    let enabled = [
        ("APP__POSTGRES__ENABLED", "true"),
        ("APP__POSTGRES__DSN", "postgres://localhost/app"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let secrets = tempfile::tempdir().unwrap();
    let mut options = LoadOptions::default();
    let defaults = load_jobs_operator_from(&options, env(&enabled)).unwrap();
    assert_eq!(password_file(&defaults), None);

    options.config = Some(write(
        &dir,
        "base.toml",
        "[postgres]\npassword_file = '/base'\n",
    ));
    let base = load_jobs_operator_from(&options, env(&enabled)).unwrap();
    assert_eq!(password_file(&base), Some(Path::new("/base")));
    for expected in ["first", "second"] {
        options.config_overlay.push(write(
            &dir,
            &format!("overlay-{expected}.toml"),
            &format!("[postgres]\npassword_file = '/{expected}'\n"),
        ));
        let overlay = load_jobs_operator_from(&options, env(&enabled)).unwrap();
        assert_eq!(
            password_file(&overlay),
            Some(Path::new(&format!("/{expected}")))
        );
    }
    write(&secrets, "APP__POSTGRES__PASSWORD_FILE", "/mounted");
    write(&secrets, "APP__POSTGRES__DSN", "postgres://localhost/app");
    options.secrets_dir = Some(secrets.path().to_owned());
    let secret =
        load_jobs_operator_from(&options, env(&[("APP__POSTGRES__ENABLED", "true")])).unwrap();
    assert_eq!(password_file(&secret), Some(Path::new("/mounted")));
    let blanked = load_jobs_operator_from(
        &options,
        env(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", ""),
        ]),
    )
    .unwrap_err();
    assert_matches!(&blanked, Error::Validate(errors) if errors.names("postgres.dsn"), "{blanked}");
}

#[test]
fn jobs_operator_keeps_postgres_validation_and_value_free_errors() {
    let options = LoadOptions::default();
    let unknown =
        load_jobs_operator_from(&options, env(&[("APP__POSTGRES__BOGUS", "1")])).unwrap_err();
    assert_matches!(unknown, Error::Deserialize(_), "{unknown}");
    let missing =
        load_jobs_operator_from(&options, env(&[("APP__POSTGRES__ENABLED", "true")])).unwrap_err();
    assert_matches!(&missing, Error::Validate(errors) if errors.names("postgres.dsn"), "{missing}");
    let budgets = load_jobs_operator_from(
        &options,
        env(&[("APP__POSTGRES__SESSION_BUDGETS", "invalid")]),
    )
    .unwrap_err();
    assert_matches!(&budgets, Error::Deserialize(_), "{budgets}");
    let secret = "fixture-private-value";
    let error =
        load_jobs_operator_from(&options, env(&[("APP__POSTGRES__ENABLED", secret)])).unwrap_err();
    assert_matches!(error, Error::Deserialize(_), "{error}");
    assert!(!error.to_string().contains(secret));
    assert!(!format!("{error:?}").contains(secret));
}

#[test]
fn jobs_operator_rejects_unknown_sections_and_typo_variables() {
    let dir = tempfile::tempdir().unwrap();
    let unknown = write(&dir, "unknown.toml", "[unrelated]\nvalue = 1\n");
    let error = load_jobs_operator_from(
        &LoadOptions {
            config: Some(unknown),
            ..LoadOptions::default()
        },
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(error, Error::Deserialize(_), "{error}");

    let typo = load_jobs_operator_from(
        &LoadOptions::default(),
        env(&[("APP__DATABASE__DSN", "postgres://u:secret-pw@localhost/db")]),
    )
    .unwrap_err();
    assert_matches!(&typo, Error::Deserialize(_), "{typo}");
    assert!(!typo.to_string().contains("secret-pw"));
    assert!(!format!("{typo:?}").contains("secret-pw"));
}

#[test]
fn jobs_operator_prescans_unrelated_sections() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[observability.otel.exporter]\notlp_headers = \"fixture-private-value\"\n",
    );
    let error = load_jobs_operator_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(error, Error::SecretInFile { .. }, "{error}");
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
    assert_matches!(error, Error::UnaddressableKey { .. }, "{error}");
    let malformed = load_jobs_operator_from(
        &LoadOptions::default(),
        env(&[("APP__UNRELATED__", "fixture-private-value")]),
    )
    .unwrap_err();
    assert_matches!(malformed, Error::MalformedEnvName { .. }, "{malformed}");
}
