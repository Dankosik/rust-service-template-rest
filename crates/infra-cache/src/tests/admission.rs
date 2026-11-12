use super::*;

#[test]
fn plaintext_without_the_allow_flag_is_refused() {
    let err = Cache::start(options(
        "redis://:hunter2@127.0.0.1:6379",
        false,
        true,
        None,
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PlaintextRefused);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn plaintext_and_a_missing_password_are_admitted_when_allowed() {
    let cache = admitted("redis://127.0.0.1:6379", true, true);
    assert_eq!(cache.server().host, "127.0.0.1");
    assert_eq!(cache.server().port, 6379);
    assert!(!cache.server().tls);
    let debug = format!("{cache:?}");
    assert!(
        debug.contains("127.0.0.1") && !debug.contains("redis://"),
        "cache debug output must identify the server without the DSN"
    );
}

#[test]
fn a_missing_password_is_refused_without_the_allow_flag() {
    let err = Cache::start(options("rediss://127.0.0.1:6379", false, false, None)).unwrap_err();
    assert_eq!(err, CacheError::UnauthenticatedRefused);
}

#[test]
fn an_authenticated_tls_address_is_admitted_without_a_server() {
    let cache = admitted("rediss://:hunter2@cache.example:6380", false, false);
    assert!(cache.server().tls);
    assert_eq!(cache.server().port, 6380);
    let rendered = format!("{cache:?} {}", cache.server().host);
    assert!(
        !rendered.contains("hunter2"),
        "cache debug output disclosed a password"
    );
}

#[test]
fn insecure_tls_fragment_is_refused() {
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379#insecure",
        false,
        true,
        None,
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InsecureTlsRefused);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn a_unix_socket_is_unsupported() {
    let err = Cache::start(options("redis+unix:///tmp/cache.sock", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::UnsupportedAddress);
}

#[test]
fn a_root_ca_on_plaintext_is_refused_before_the_file_is_read() {
    let err = Cache::start(options(
        "redis://127.0.0.1:6379",
        true,
        true,
        Some(PathBuf::from("/no/such/cache-ca.pem")),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::CaRequiresTls);
}

#[test]
fn a_missing_root_ca_file_reports_only_the_io_kind() {
    let missing = PathBuf::from("/no/such/cache-ca.pem");
    let err = Cache::start(options(
        "rediss://:secret@127.0.0.1:6379",
        false,
        false,
        Some(missing),
    ))
    .unwrap_err();
    std::assert_matches!(err, CacheError::CaFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("secret"));
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_root_ca_without_a_pem_certificate_is_invalid() {
    let file = tempfile::NamedTempFile::new().expect("temp ca");
    std::fs::write(file.path(), b"not a certificate").expect("write ca");
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_root_ca_with_a_certificate_header_and_invalid_base64_is_invalid() {
    let file = tempfile::NamedTempFile::new().expect("temp ca");
    std::fs::write(
        file.path(),
        b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
    )
    .expect("write ca");
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

/// A PEM file holding one block of `der` under `label`.
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn write_pem(label: &str, der: &[u8]) -> tempfile::NamedTempFile {
    use base64::Engine;

    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut body = String::new();
    for line in encoded.as_bytes().chunks(64) {
        body.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
        body.push('\n');
    }
    let pem = format!("-----BEGIN {label}-----\n{body}-----END {label}-----\n");
    let file = tempfile::NamedTempFile::new().expect("temp pem");
    std::fs::write(file.path(), pem).expect("write pem");
    file
}

/// A self-signed certificate and its PKCS #8 key, each in its own PEM file.
fn client_identity() -> (tempfile::NamedTempFile, tempfile::NamedTempFile) {
    let key = rcgen::KeyPair::generate().expect("client key");
    let certificate = rcgen::CertificateParams::new(vec!["cache-client".to_owned()])
        .expect("client certificate params")
        .self_signed(&key)
        .expect("client certificate");
    (
        write_pem("CERTIFICATE", certificate.der()),
        write_pem("PRIVATE KEY", &key.serialize_der()),
    )
}

fn with_client_certificate(dsn: &str, cert_path: PathBuf, key_path: PathBuf) -> CacheOptions {
    CacheOptions {
        client_certificate: Some(ClientCertificate {
            cert_path,
            key_path,
        }),
        ..options(dsn, true, false, None)
    }
}

const TLS_DSN: &str = "rediss://:hunter2@127.0.0.1:6379";

#[test]
fn a_client_certificate_with_its_key_is_admitted_without_dialing() {
    let (cert, key) = client_identity();
    on_runtime(|| {
        Cache::start(with_client_certificate(
            TLS_DSN,
            cert.path().to_path_buf(),
            key.path().to_path_buf(),
        ))
        .expect("a matching pair is admitted")
    });
}

#[test]
fn a_client_certificate_on_plaintext_is_refused_before_the_files_are_read() {
    let err = Cache::start(with_client_certificate(
        "redis://:hunter2@127.0.0.1:6379",
        PathBuf::from("/no/such/client.crt"),
        PathBuf::from("/no/such/client.key"),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::ClientCertificateRequiresTls);
}

#[test]
fn a_missing_client_certificate_or_key_file_reports_only_the_io_kind() {
    let (cert, key) = client_identity();
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        PathBuf::from("/no/such/client.crt"),
        key.path().to_path_buf(),
    ))
    .unwrap_err();
    std::assert_matches!(err, CacheError::ClientCertificateFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("/no/such"), "{err:?}");

    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        PathBuf::from("/no/such/client.key"),
    ))
    .unwrap_err();
    std::assert_matches!(err, CacheError::ClientKeyFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("/no/such"), "{err:?}");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_client_certificate_file_without_a_pem_certificate_is_invalid() {
    let (_, key) = client_identity();
    let file = tempfile::NamedTempFile::new().expect("temp certificate");
    std::fs::write(file.path(), b"not a certificate").expect("write certificate");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        file.path().to_path_buf(),
        key.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientCertificate);
}

#[test]
fn a_client_certificate_in_valid_pem_but_not_x509_blames_the_certificate() {
    let (_, key) = client_identity();
    let truncated = write_pem("CERTIFICATE", b"not a DER certificate");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        truncated.path().to_path_buf(),
        key.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientCertificate);
}

#[test]
fn a_root_ca_in_valid_pem_but_not_x509_is_invalid() {
    let truncated = write_pem("CERTIFICATE", b"not a DER certificate");
    let err = Cache::start(options(
        TLS_DSN,
        false,
        false,
        Some(truncated.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_client_key_that_is_unusable_or_belongs_to_another_certificate_is_invalid() {
    let (cert, _) = client_identity();
    let (_, other_key) = client_identity();
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        other_key.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);

    let garbage = tempfile::NamedTempFile::new().expect("temp key");
    std::fs::write(garbage.path(), b"not a key").expect("write key");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        garbage.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn an_unparseable_dsn_does_not_echo_the_password() {
    let err = Cache::start(options("not a url hunter2", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::InvalidDsn);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn an_admitted_dsn_outside_a_runtime_is_refused() {
    let err = Cache::start(options("redis://127.0.0.1:6379", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::NoRuntime);
}

#[test]
fn a_tokio_runtime_without_timers_is_refused_at_startup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .expect("Tokio runtime with I/O");
    let _entered = runtime.enter();
    let err = Cache::start(options("redis://127.0.0.1:6379", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::NoTimeDriver);
}

#[test]
fn options_debug_redacts_the_dsn() {
    let rendered = format!(
        "{:?}",
        options("redis://:hunter2@127.0.0.1:6379", true, true, None)
    );
    assert!(
        !rendered.contains("hunter2"),
        "cache debug output disclosed a password"
    );
}

#[test]
#[should_panic(expected = "cache namespace")]
fn a_namespace_name_outside_the_grammar_panics() {
    let cache = admitted("redis://127.0.0.1:6379", true, true);
    let _ = cache.namespace("Bad-Name");
}

#[test]
fn a_namespace_name_at_the_length_bound_is_accepted() {
    let cache = admitted("redis://127.0.0.1:6379", true, true);
    let name = "a".repeat(64);
    let name = Box::leak(name.into_boxed_str());
    let _ = cache.namespace(name);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_password_file_admits_a_dsn_without_a_password() {
    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), "hunter2\n").expect("write password");
    // No password in the DSN and no `allow_unauthenticated`.
    on_runtime(|| {
        Cache::start(with_password_file(
            "redis://127.0.0.1:6379",
            file.path().to_path_buf(),
        ))
        .expect("the file is the password source")
    });
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_password_in_the_dsn_and_a_password_file_are_refused_together() {
    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), "from-file").expect("write password");
    let err = Cache::start(with_password_file(
        "redis://:hunter2@127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordInDsn);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn an_unreadable_or_empty_password_file_fails_admission() {
    let missing = PathBuf::from("/no/such/cache-password");
    let err = Cache::start(with_password_file("redis://127.0.0.1:6379", missing)).unwrap_err();
    std::assert_matches!(err, CacheError::PasswordFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(!err.to_string().contains("/no/such"), "{err}");

    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), "\n").expect("write password");
    let err = Cache::start(with_password_file(
        "redis://127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordFileEmpty);
}
