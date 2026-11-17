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
    assert_no_leak(&err, &["hunter2"]);
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
fn valkey_schemes_are_admitted_like_redis_schemes() {
    assert!(!admitted("valkey://127.0.0.1:6379", true, true).server().tls);
    assert!(
        admitted("valkeys://:hunter2@cache.example:6380", false, false)
            .server()
            .tls
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
    assert_no_leak(&err, &["hunter2"]);
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
    assert_no_leak(&err, &["secret"]);
}

#[test]
fn a_root_ca_without_a_pem_certificate_is_invalid() {
    let file = temp_file(b"not a certificate");
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert_no_leak(&err, &["hunter2"]);
}

#[test]
fn a_root_ca_with_a_certificate_header_and_invalid_base64_is_invalid() {
    let file = temp_file(b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n");
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert_no_leak(&err, &["hunter2"]);
}

fn write_pem(label: &str, der: &[u8]) -> tempfile::NamedTempFile {
    let pem = pem::encode_config(
        &pem::Pem::new(label, der),
        pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
    );
    temp_file(pem)
}

#[test]
fn pem_fixtures_preserve_labels_and_wrap_at_64_columns_with_lf() {
    let empty = write_pem("CERTIFICATE", &[]);
    assert_eq!(
        std::io::read_to_string(empty.reopen().unwrap()).unwrap(),
        "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n",
    );
    let key = write_pem("PRIVATE KEY", &[0; 49]);
    assert_eq!(
        std::io::read_to_string(key.reopen().unwrap()).unwrap(),
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\nAA==\n-----END PRIVATE KEY-----\n",
            "A".repeat(64)
        ),
    );
}

fn client_identity() -> (tempfile::NamedTempFile, tempfile::NamedTempFile) {
    let identity = rcgen::generate_simple_self_signed(["cache-client".to_owned()])
        .expect("client certificate");
    (
        write_pem("CERTIFICATE", identity.cert.der()),
        write_pem("PRIVATE KEY", &identity.signing_key.serialize_der()),
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
    assert_no_leak(&err, &["/no/such"]);

    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        PathBuf::from("/no/such/client.key"),
    ))
    .unwrap_err();
    std::assert_matches!(err, CacheError::ClientKeyFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert_no_leak(&err, &["/no/such"]);
}

#[test]
fn a_client_certificate_file_without_a_pem_certificate_is_invalid() {
    let (_, key) = client_identity();
    let file = temp_file(b"not a certificate");
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

    let garbage = temp_file(b"not a key");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        garbage.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);
    assert_no_leak(&err, &["hunter2"]);
}

#[test]
fn an_unparsable_dsn_does_not_echo_the_password() {
    let err = Cache::start(options("not a url hunter2", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::InvalidDsn);
    assert_no_leak(&err, &["hunter2"]);
}

#[test]
fn an_admitted_dsn_outside_a_runtime_is_refused() {
    let err = Cache::start(options("redis://127.0.0.1:6379", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::NoRuntime);
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
fn namespace_names_follow_the_grammar() {
    let longest = format!("a{}", "_9".repeat(31) + "z");
    let too_long = format!("{longest}a");
    let longest: &'static str = longest.leak();
    let too_long: &'static str = too_long.leak();
    for accepted in ["a", "a_1", "abc_def9", longest] {
        assert!(Namespace::new(accepted).is_some(), "{accepted:?} is valid");
    }
    for refused in [
        "", "1a", "_a", "Ab", "aB", "a-b", "a:b", "a b", "é", too_long,
    ] {
        assert!(Namespace::new(refused).is_none(), "{refused:?} is invalid");
    }
}

#[test]
fn a_password_file_admits_a_dsn_without_a_password() {
    let file = temp_file("hunter2\n");
    on_runtime(|| {
        Cache::start(with_password_file(
            "redis://127.0.0.1:6379",
            file.path().to_path_buf(),
        ))
        .expect("the file is the password source")
    });
}

#[test]
fn a_password_in_the_dsn_and_a_password_file_are_refused_together() {
    let file = temp_file("from-file");
    let err = Cache::start(with_password_file(
        "redis://:hunter2@127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordInDsn);
    assert_no_leak(&err, &["hunter2"]);
}

#[test]
fn an_unreadable_or_empty_password_file_fails_admission() {
    let missing = PathBuf::from("/no/such/cache-password");
    let err = Cache::start(with_password_file("redis://127.0.0.1:6379", missing)).unwrap_err();
    std::assert_matches!(err, CacheError::PasswordFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(!err.to_string().contains("/no/such"), "{err}");

    let file = temp_file("\n");
    let err = Cache::start(with_password_file(
        "redis://127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordFileEmpty);
}
