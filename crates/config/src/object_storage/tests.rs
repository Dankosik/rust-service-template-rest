use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, assert_secret_in_file, env, load_env, write};
use crate::validate::{ValidationErrors, admit_with};
use std::assert_matches;

fn railway() -> ObjectStorageInput {
    ObjectStorageInput {
        provider: ProviderKind::Railway,
        bucket: "results-jdhhd8oe18xi".to_owned(),
        endpoint: "https://t3.storageapi.dev".to_owned(),
        access_key_id: "tid_example".to_owned(),
        secret_access_key: Some(SecretString::from("hunter2".to_owned())),
        ..ObjectStorageInput::default()
    }
}

fn amazon() -> ObjectStorageInput {
    ObjectStorageInput {
        provider: ProviderKind::AmazonS3,
        endpoint: String::new(),
        region: "eu-central-1".to_owned(),
        expected_bucket_owner: "123456789012".to_owned(),
        ..railway()
    }
}

fn admit(
    input: ObjectStorageInput,
    env: &str,
) -> Result<Option<ObjectStorageConfig>, ValidationErrors> {
    admit_with(|v| input.admit(env, v))
}

fn keys(input: ObjectStorageInput, env: &str) -> Vec<String> {
    admit(input, env)
        .unwrap_err()
        .iter()
        .map(|error| error.key.clone())
        .collect()
}

fn key_of(input: ObjectStorageInput, env: &str) -> String {
    let mut keys = keys(input, env);
    assert_eq!(keys.len(), 1, "{keys:?}");
    keys.remove(0)
}

#[test]
fn defaults_are_inert() {
    let input = ObjectStorageInput::default();
    assert_eq!(input.provider, ProviderKind::None);
    assert_eq!(input.max_object_bytes, ByteSize::mib(8));
    assert_eq!(input.max_concurrency, 8);
    assert_eq!(input.operation_timeout, Duration::from_secs(5));
    assert!(admit(input, "production").unwrap().is_none());
}

#[test]
fn an_active_provider_needs_bucket_and_credentials() {
    let admitted = admit(railway(), "production").unwrap().unwrap();
    assert_eq!(admitted.bucket, "results-jdhhd8oe18xi");
    assert_eq!(
        admitted.provider,
        ObjectStorageProvider::Railway {
            endpoint: "https://t3.storageapi.dev".to_owned(),
            region: None,
        }
    );
    for (mutate, key) in [
        (
            (|input: &mut ObjectStorageInput| input.bucket.clear()) as fn(&mut _),
            "object_storage.bucket",
        ),
        (
            |input| input.access_key_id.clear(),
            "object_storage.access_key_id",
        ),
        (
            |input| input.secret_access_key = None,
            "object_storage.secret_access_key",
        ),
        (|input| input.endpoint.clear(), "object_storage.endpoint"),
    ] {
        let mut input = railway();
        mutate(&mut input);
        assert_eq!(key_of(input, "production"), key);
    }
}

#[test]
fn every_violated_rule_is_reported_together() {
    let input = ObjectStorageInput {
        bucket: String::new(),
        endpoint: String::new(),
        access_key_id: String::new(),
        max_concurrency: 0,
        ..railway()
    };
    assert_eq!(
        keys(input, "production"),
        [
            "object_storage.max_concurrency",
            "object_storage.endpoint",
            "object_storage.bucket",
            "object_storage.access_key_id"
        ]
    );
}

#[test]
fn blank_optional_regions_are_absent() {
    let mut input = railway();
    input.region = "  ".to_owned();
    let admitted = admit(input, "production").unwrap().unwrap();
    assert_matches!(
        admitted.provider,
        ObjectStorageProvider::Railway { region: None, .. }
    );
}

#[test]
fn providers_accept_only_their_own_keys() {
    let mut other = railway();
    other.provider = ProviderKind::AmazonS3;
    let errors = admit(other.clone(), "production").unwrap_err();
    let error = errors.iter().next().unwrap();
    assert_eq!(error.key, "object_storage.endpoint");
    assert_eq!(
        error.message,
        "must be empty when object_storage.provider = amazon_s3"
    );
    assert!(errors.names("object_storage.region"));
    assert!(errors.names("object_storage.expected_bucket_owner"));
    other.endpoint.clear();
    assert_eq!(
        keys(other.clone(), "production"),
        [
            "object_storage.region",
            "object_storage.expected_bucket_owner"
        ]
    );
    other.region = "eu-central-1".to_owned();
    assert_eq!(
        key_of(other, "production"),
        "object_storage.expected_bucket_owner"
    );
    assert_eq!(
        admit(amazon(), "production").unwrap().unwrap().provider,
        ObjectStorageProvider::AmazonS3 {
            region: "eu-central-1".to_owned(),
            expected_bucket_owner: "123456789012".to_owned(),
        }
    );

    let mut r2 = railway();
    r2.provider = ProviderKind::CloudflareR2;
    r2.region = "eu-central-1".to_owned();
    assert_eq!(key_of(r2.clone(), "production"), "object_storage.region");
    r2.region = "auto".to_owned();
    assert_eq!(
        admit(r2.clone(), "production").unwrap().unwrap().provider,
        ObjectStorageProvider::CloudflareR2 {
            endpoint: "https://t3.storageapi.dev".to_owned(),
        }
    );
    r2.expected_bucket_owner = "123456789012".to_owned();
    let errors = admit(r2, "production").unwrap_err();
    let error = errors.iter().next().unwrap();
    assert_eq!(error.key, "object_storage.expected_bucket_owner");
    assert_eq!(
        error.message,
        "is accepted only when object_storage.provider = amazon_s3"
    );
}

#[test]
fn cloudflare_r2_trims_blank_and_auto_regions() {
    let mut r2 = railway();
    r2.provider = ProviderKind::CloudflareR2;
    for accepted in ["", "   ", "auto", " auto ", "\tauto\n"] {
        r2.region = accepted.to_owned();
        admit(r2.clone(), "production").unwrap();
    }
    for rejected in ["eu-central-1", " eu-central-1 ", "AUTO"] {
        r2.region = rejected.to_owned();
        assert_eq!(key_of(r2.clone(), "production"), "object_storage.region");
    }
}

#[test]
fn workload_identity_is_amazon_only_and_takes_no_access_key() {
    let mut input = amazon();
    input.credentials = CredentialsKind::WorkloadIdentity;
    assert_eq!(
        keys(input.clone(), "production"),
        [
            "object_storage.access_key_id",
            "object_storage.secret_access_key"
        ]
    );
    input.access_key_id.clear();
    assert_eq!(
        key_of(input.clone(), "production"),
        "object_storage.secret_access_key"
    );
    input.secret_access_key = None;
    assert_matches!(
        admit(input, "production").unwrap().unwrap().credentials,
        ObjectStorageCredentials::WorkloadIdentity
    );

    let mut other = railway();
    other.credentials = CredentialsKind::WorkloadIdentity;
    other.access_key_id.clear();
    other.secret_access_key = None;
    assert_eq!(key_of(other, "production"), "object_storage.credentials");
}

#[test]
fn path_style_belongs_to_the_generic_provider() {
    let mut generic = railway();
    generic.provider = ProviderKind::S3Compatible;
    generic.path_style = true;
    assert_matches!(
        admit(generic.clone(), "production")
            .unwrap()
            .unwrap()
            .provider,
        ObjectStorageProvider::S3Compatible {
            path_style: true,
            ..
        }
    );
    generic.expected_bucket_owner = "123456789012".to_owned();
    assert_eq!(
        key_of(generic, "production"),
        "object_storage.expected_bucket_owner"
    );

    let mut other = railway();
    other.path_style = true;
    let errors = admit(other, "production").unwrap_err();
    let error = errors.iter().next().unwrap();
    assert_eq!(error.key, "object_storage.path_style");
    assert_eq!(
        error.message,
        "must be false unless object_storage.provider = s3_compatible"
    );
}

#[test]
fn the_emulator_provider_is_local_only() {
    let mut local = railway();
    local.provider = ProviderKind::Local;
    assert_eq!(
        key_of(local.clone(), "production"),
        "object_storage.provider"
    );
    for env in ["local", "development"] {
        assert_matches!(
            admit(local.clone(), env).unwrap().unwrap().provider,
            ObjectStorageProvider::Local { .. }
        );
    }
}

#[test]
fn limits_are_bounded_even_when_inert() {
    for (mutate, key) in [
        (
            (|input: &mut ObjectStorageInput| input.max_object_bytes = ByteSize::b(0))
                as fn(&mut _),
            "object_storage.max_object_bytes",
        ),
        (
            |input| input.max_object_bytes = ByteSize::b(MAX_OBJECT_BYTES_CEILING + 1),
            "object_storage.max_object_bytes",
        ),
        (
            |input| input.max_concurrency = 0,
            "object_storage.max_concurrency",
        ),
        (
            |input| input.max_concurrency = 513,
            "object_storage.max_concurrency",
        ),
        (
            |input| input.operation_timeout = Duration::from_millis(999),
            "object_storage.operation_timeout",
        ),
        (
            |input| input.operation_timeout = Duration::from_secs(901),
            "object_storage.operation_timeout",
        ),
    ] {
        let mut input = ObjectStorageInput::default();
        mutate(&mut input);
        assert_eq!(key_of(input, "production"), key);
    }
    assert_eq!(MAX_OBJECT_BYTES_CEILING, 5_363_466_240);
}

#[test]
fn debug_output_redacts_the_secret() {
    let rendered = format!("{:?}", admit(railway(), "production").unwrap());
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
fn object_storage_environment_maps_railway_bucket_variables() {
    use secrecy::ExposeSecret as _;

    let cfg = load_env(&[
        ("APP__OBJECT_STORAGE__PROVIDER", "railway"),
        ("APP__OBJECT_STORAGE__BUCKET", "results-jdhhd8oe18xi"),
        ("APP__OBJECT_STORAGE__ENDPOINT", "https://t3.storageapi.dev"),
        ("APP__OBJECT_STORAGE__REGION", "auto"),
        ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "tid_example"),
        ("APP__OBJECT_STORAGE__SECRET_ACCESS_KEY", "hunter2"),
        ("APP__OBJECT_STORAGE__MAX_OBJECT_BYTES", "2 MiB"),
        ("APP__OBJECT_STORAGE__MAX_CONCURRENCY", "32"),
        ("APP__OBJECT_STORAGE__OPERATION_TIMEOUT", "10s"),
    ]);
    let storage = cfg.object_storage.as_ref().unwrap();
    assert_eq!(
        storage.provider,
        crate::ObjectStorageProvider::Railway {
            endpoint: "https://t3.storageapi.dev".to_owned(),
            region: Some("auto".to_owned()),
        }
    );
    assert_eq!(storage.bucket, "results-jdhhd8oe18xi");
    let crate::ObjectStorageCredentials::AccessKey {
        access_key_id,
        secret_access_key,
    } = &storage.credentials
    else {
        panic!("expected access key credentials");
    };
    assert_eq!(access_key_id, "tid_example");
    assert_eq!(secret_access_key.expose_secret(), "hunter2");
    assert_eq!(storage.max_object_bytes.as_u64(), 2 * 1024 * 1024);
    assert_eq!(storage.max_concurrency, 32);
    assert_eq!(storage.operation_timeout, Duration::from_secs(10));
    assert!(!format!("{cfg:?}").contains("hunter2"));
}

#[test]
fn object_storage_environment_selects_credentials_and_addressing() {
    let amazon = load_env(&[
        ("APP__OBJECT_STORAGE__PROVIDER", "amazon_s3"),
        ("APP__OBJECT_STORAGE__BUCKET", "document-results"),
        ("APP__OBJECT_STORAGE__REGION", "eu-central-1"),
        ("APP__OBJECT_STORAGE__EXPECTED_BUCKET_OWNER", "123456789012"),
        ("APP__OBJECT_STORAGE__CREDENTIALS", "workload_identity"),
    ]);
    assert_matches!(
        amazon.object_storage.as_ref().unwrap().credentials,
        crate::ObjectStorageCredentials::WorkloadIdentity
    );

    let generic = load_env(&[
        ("APP__OBJECT_STORAGE__PROVIDER", "s3_compatible"),
        ("APP__OBJECT_STORAGE__BUCKET", "document-results"),
        (
            "APP__OBJECT_STORAGE__ENDPOINT",
            "https://ceph.internal.example:8443",
        ),
        ("APP__OBJECT_STORAGE__PATH_STYLE", "true"),
        ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "example"),
        ("APP__OBJECT_STORAGE__SECRET_ACCESS_KEY", "hunter2"),
    ]);
    assert_eq!(
        generic.object_storage.as_ref().unwrap().provider,
        crate::ObjectStorageProvider::S3Compatible {
            endpoint: "https://ceph.internal.example:8443".to_owned(),
            region: None,
            path_style: true,
        }
    );
}

#[test]
fn object_storage_secret_in_a_file_is_refused() {
    assert_secret_in_file(
        "[object_storage]\nsecret_access_key = \"hunter2\"\n",
        "object_storage.secret_access_key",
    );
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
    assert_matches!(
        cfg.object_storage.as_ref().unwrap().credentials,
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
