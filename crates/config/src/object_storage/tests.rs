use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};
use std::assert_matches;

fn railway() -> ObjectStorageConfig {
    ObjectStorageConfig {
        provider: ObjectStorageProvider::Railway,
        bucket: "results-jdhhd8oe18xi".to_owned(),
        endpoint: "https://t3.storageapi.dev".to_owned(),
        access_key_id: "tid_example".to_owned(),
        secret_access_key: Some(SecretString::from("hunter2".to_owned())),
        ..ObjectStorageConfig::default()
    }
}

fn key_of(config: &ObjectStorageConfig, env: &str) -> String {
    config.validate(env).unwrap_err().key
}

#[test]
fn defaults_are_inert() {
    let config = ObjectStorageConfig::default();
    assert_eq!(config.provider, ObjectStorageProvider::None);
    assert_eq!(config.max_object_bytes, ByteSize::mib(8));
    assert_eq!(config.max_concurrency, 8);
    assert_eq!(config.operation_timeout, Duration::from_secs(5));
    config.validate("production").unwrap();
}

#[test]
fn an_active_provider_needs_bucket_and_credentials() {
    railway().validate("production").unwrap();
    for (mutate, key) in [
        (
            (|config: &mut ObjectStorageConfig| config.bucket.clear()) as fn(&mut _),
            "object_storage.bucket",
        ),
        (
            |config| config.access_key_id.clear(),
            "object_storage.access_key_id",
        ),
        (
            |config| config.secret_access_key = None,
            "object_storage.secret_access_key",
        ),
        (|config| config.endpoint.clear(), "object_storage.endpoint"),
    ] {
        let mut config = railway();
        mutate(&mut config);
        assert_eq!(key_of(&config, "production"), key);
    }
}

#[test]
fn providers_accept_only_their_own_keys() {
    let mut amazon = railway();
    amazon.provider = ObjectStorageProvider::AmazonS3;
    let error = amazon.validate("production").unwrap_err();
    assert_eq!(error.key, "object_storage.endpoint");
    assert_eq!(
        error.message,
        "must be empty when object_storage.provider = amazon_s3"
    );
    amazon.endpoint.clear();
    assert_eq!(key_of(&amazon, "production"), "object_storage.region");
    amazon.region = "eu-central-1".to_owned();
    assert_eq!(
        key_of(&amazon, "production"),
        "object_storage.expected_bucket_owner"
    );
    amazon.expected_bucket_owner = "123456789012".to_owned();
    amazon.validate("production").unwrap();

    let mut r2 = railway();
    r2.provider = ObjectStorageProvider::CloudflareR2;
    r2.region = "eu-central-1".to_owned();
    assert_eq!(key_of(&r2, "production"), "object_storage.region");
    r2.region = "auto".to_owned();
    r2.validate("production").unwrap();
    r2.expected_bucket_owner = "123456789012".to_owned();
    let error = r2.validate("production").unwrap_err();
    assert_eq!(error.key, "object_storage.expected_bucket_owner");
    assert_eq!(
        error.message,
        "is accepted only when object_storage.provider = amazon_s3"
    );
}

#[test]
fn workload_identity_is_amazon_only_and_takes_no_access_key() {
    let mut amazon = railway();
    amazon.provider = ObjectStorageProvider::AmazonS3;
    amazon.endpoint.clear();
    amazon.region = "eu-central-1".to_owned();
    amazon.expected_bucket_owner = "123456789012".to_owned();
    amazon.credentials = ObjectStorageCredentials::WorkloadIdentity;
    assert_eq!(
        key_of(&amazon, "production"),
        "object_storage.access_key_id"
    );
    amazon.access_key_id.clear();
    assert_eq!(
        key_of(&amazon, "production"),
        "object_storage.secret_access_key"
    );
    amazon.secret_access_key = None;
    amazon.validate("production").unwrap();

    let mut other = railway();
    other.credentials = ObjectStorageCredentials::WorkloadIdentity;
    other.access_key_id.clear();
    other.secret_access_key = None;
    assert_eq!(key_of(&other, "production"), "object_storage.credentials");
}

#[test]
fn path_style_belongs_to_the_generic_provider() {
    let mut generic = railway();
    generic.provider = ObjectStorageProvider::S3Compatible;
    generic.path_style = true;
    generic.validate("production").unwrap();
    generic.expected_bucket_owner = "123456789012".to_owned();
    assert_eq!(
        key_of(&generic, "production"),
        "object_storage.expected_bucket_owner"
    );

    let mut other = railway();
    other.path_style = true;
    let error = other.validate("production").unwrap_err();
    assert_eq!(error.key, "object_storage.path_style");
    assert_eq!(
        error.message,
        "must be false unless object_storage.provider = s3_compatible"
    );
}

#[test]
fn the_emulator_provider_is_local_only() {
    let mut local = railway();
    local.provider = ObjectStorageProvider::Local;
    assert_eq!(key_of(&local, "production"), "object_storage.provider");
    local.validate("local").unwrap();
    local.validate("development").unwrap();
}

#[test]
fn limits_are_bounded_even_when_inert() {
    for (mutate, key) in [
        (
            (|config: &mut ObjectStorageConfig| config.max_object_bytes = ByteSize::b(0))
                as fn(&mut _),
            "object_storage.max_object_bytes",
        ),
        (
            |config| config.max_object_bytes = ByteSize::b(MAX_OBJECT_BYTES_CEILING + 1),
            "object_storage.max_object_bytes",
        ),
        (
            |config| config.max_concurrency = 0,
            "object_storage.max_concurrency",
        ),
        (
            |config| config.max_concurrency = 513,
            "object_storage.max_concurrency",
        ),
        (
            |config| config.operation_timeout = Duration::from_millis(999),
            "object_storage.operation_timeout",
        ),
        (
            |config| config.operation_timeout = Duration::from_secs(901),
            "object_storage.operation_timeout",
        ),
    ] {
        let mut config = ObjectStorageConfig::default();
        mutate(&mut config);
        assert_eq!(key_of(&config, "production"), key);
    }
    assert_eq!(MAX_OBJECT_BYTES_CEILING, 5_363_466_240);
}

#[test]
fn debug_output_redacts_the_secret() {
    let rendered = format!("{:?}", railway());
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
fn object_storage_environment_maps_railway_bucket_variables() {
    use secrecy::ExposeSecret as _;

    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__OBJECT_STORAGE__PROVIDER", "railway"),
            ("APP__OBJECT_STORAGE__BUCKET", "results-jdhhd8oe18xi"),
            ("APP__OBJECT_STORAGE__ENDPOINT", "https://t3.storageapi.dev"),
            ("APP__OBJECT_STORAGE__REGION", "auto"),
            ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "tid_example"),
            ("APP__OBJECT_STORAGE__SECRET_ACCESS_KEY", "hunter2"),
            ("APP__OBJECT_STORAGE__MAX_OBJECT_BYTES", "2 MiB"),
            ("APP__OBJECT_STORAGE__MAX_CONCURRENCY", "32"),
            ("APP__OBJECT_STORAGE__OPERATION_TIMEOUT", "10s"),
        ]),
    )
    .unwrap();
    let storage = &cfg.object_storage;
    assert_eq!(storage.provider, crate::ObjectStorageProvider::Railway);
    assert_eq!(storage.bucket, "results-jdhhd8oe18xi");
    assert_eq!(
        storage.secret_access_key.as_ref().unwrap().expose_secret(),
        "hunter2"
    );
    assert_eq!(storage.max_object_bytes.as_u64(), 2 * 1024 * 1024);
    assert_eq!(storage.max_concurrency, 32);
    assert_eq!(storage.operation_timeout, Duration::from_secs(10));
    assert!(!format!("{cfg:?}").contains("hunter2"));
}

#[test]
fn object_storage_environment_selects_credentials_and_addressing() {
    let amazon = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__OBJECT_STORAGE__PROVIDER", "amazon_s3"),
            ("APP__OBJECT_STORAGE__BUCKET", "document-results"),
            ("APP__OBJECT_STORAGE__REGION", "eu-central-1"),
            ("APP__OBJECT_STORAGE__EXPECTED_BUCKET_OWNER", "123456789012"),
            ("APP__OBJECT_STORAGE__CREDENTIALS", "workload_identity"),
        ]),
    )
    .unwrap();
    assert_eq!(
        amazon.object_storage.credentials,
        crate::ObjectStorageCredentials::WorkloadIdentity
    );

    let generic = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__OBJECT_STORAGE__PROVIDER", "s3_compatible"),
            ("APP__OBJECT_STORAGE__BUCKET", "document-results"),
            (
                "APP__OBJECT_STORAGE__ENDPOINT",
                "https://ceph.internal.example:8443",
            ),
            ("APP__OBJECT_STORAGE__PATH_STYLE", "true"),
            ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "example"),
            ("APP__OBJECT_STORAGE__SECRET_ACCESS_KEY", "hunter2"),
        ]),
    )
    .unwrap();
    assert_eq!(
        generic.object_storage.provider,
        crate::ObjectStorageProvider::S3Compatible
    );
    assert!(generic.object_storage.path_style);
}

#[test]
fn object_storage_secret_in_a_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[object_storage]\nsecret_access_key = \"hunter2\"\n",
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
    assert_matches!(&err, Error::SecretInFile { key, .. } if key == "object_storage.secret_access_key", "{err}");
}

#[test]
fn object_storage_credentials_selector_in_a_file_is_public() {
    let dir = tempfile::tempdir().unwrap();
    let selected = write(
        &dir,
        "selected.toml",
        "[object_storage]\nprovider = \"amazon_s3\"\nbucket = \"document-results\"\nregion = \"eu-central-1\"\nexpected_bucket_owner = \"123456789012\"\ncredentials = \"workload_identity\"\n",
    );
    let cfg = load_from(
        &LoadOptions {
            config: Some(selected),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    assert_eq!(
        cfg.object_storage.credentials,
        crate::ObjectStorageCredentials::WorkloadIdentity
    );
}

#[test]
fn object_storage_unknown_provider_fails_loading() {
    assert_matches!(
        load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__OBJECT_STORAGE__PROVIDER", "minio")]),
        ),
        Err(Error::Deserialize(_))
    );
}
