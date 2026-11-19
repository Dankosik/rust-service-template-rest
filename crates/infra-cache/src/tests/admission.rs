use super::*;

#[tokio::test]
async fn plaintext_without_the_allow_flag_is_refused() {
    let err = Cache::start(options(
        "redis://:hunter2@127.0.0.1:6379",
        false,
        true,
        None,
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::PlaintextRefused);
    assert_no_leak(&err, &["hunter2"]);
}

#[tokio::test]
async fn plaintext_and_a_missing_password_are_admitted_when_allowed() {
    let cache = admitted("redis://127.0.0.1:6379", true, true).await;
    assert_eq!(cache.link.server.host, "127.0.0.1");
    assert_eq!(cache.link.server.port, 6379);
    assert!(!cache.link.server.tls);
    let debug = format!("{cache:?}");
    assert!(
        debug.contains("127.0.0.1") && !debug.contains("redis://"),
        "cache debug output must identify the server without the DSN"
    );
}

#[tokio::test]
async fn a_missing_password_is_refused_without_the_allow_flag() {
    let err = Cache::start(options("rediss://127.0.0.1:6379", false, false, None))
        .await
        .unwrap_err();
    assert_eq!(err, CacheError::UnauthenticatedRefused);
}

#[tokio::test]
async fn an_authenticated_tls_address_is_admitted_without_a_server() {
    let cache = admitted("rediss://:hunter2@cache.example:6380", false, false).await;
    assert!(cache.link.server.tls);
    assert_eq!(cache.link.server.port, 6380);
    let rendered = format!("{cache:?} {}", cache.link.server.host);
    assert!(
        !rendered.contains("hunter2"),
        "cache debug output disclosed a password"
    );
}

#[tokio::test]
async fn valkey_schemes_are_admitted_like_redis_schemes() {
    assert!(
        !admitted("valkey://127.0.0.1:6379", true, true)
            .await
            .link
            .server
            .tls
    );
    assert!(
        admitted("valkeys://:hunter2@cache.example:6380", false, false)
            .await
            .link
            .server
            .tls
    );
}

#[tokio::test]
async fn insecure_tls_fragment_is_refused() {
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379#insecure",
        false,
        true,
        None,
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InsecureTlsRefused);
    assert_no_leak(&err, &["hunter2"]);
}

#[tokio::test]
async fn a_unix_socket_is_unsupported() {
    let err = Cache::start(options("redis+unix:///tmp/cache.sock", true, true, None))
        .await
        .unwrap_err();
    assert_eq!(err, CacheError::UnsupportedAddress);
}

#[tokio::test]
async fn a_root_ca_on_plaintext_is_refused_before_the_file_is_read() {
    let err = Cache::start(options(
        "redis://127.0.0.1:6379",
        true,
        true,
        Some(PathBuf::from("/no/such/cache-ca.pem")),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::CaRequiresTls);
}

#[tokio::test]
async fn a_missing_root_ca_file_reports_only_the_io_kind() {
    let missing = PathBuf::from("/no/such/cache-ca.pem");
    let err = Cache::start(options(
        "rediss://:secret@127.0.0.1:6379",
        false,
        false,
        Some(missing),
    ))
    .await
    .unwrap_err();
    std::assert_matches!(err, CacheError::CaFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert_no_leak(&err, &["secret"]);
}

#[tokio::test]
async fn a_root_ca_without_a_pem_certificate_is_invalid() {
    let file = temp_file(b"not a certificate");
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert_no_leak(&err, &["hunter2"]);
}

#[tokio::test]
async fn a_root_ca_with_a_certificate_header_and_invalid_base64_is_invalid() {
    let file = temp_file(b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n");
    let err = Cache::start(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .await
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

#[cfg(unix)]
pub(super) fn with_unserviced_fifo(test: impl Future<Output = ()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("FIFO fixture runtime");
    let runtime = std::mem::DropGuard::new(runtime, |runtime| {
        if std::thread::panicking() {
            runtime.shutdown_background();
        }
    });
    runtime.block_on(test);
}

#[cfg(unix)]
#[test]
fn configured_files_accept_regular_symlink_targets_and_refuse_fifos_without_a_writer() {
    use nix::sys::stat::Mode;
    use std::io::ErrorKind;
    use std::os::unix::fs::symlink;

    with_unserviced_fifo(async {
        let directory = tempfile::tempdir().unwrap();
        let (cert, key) = client_identity();
        let password = temp_file(ephemeral_password());
        let links = ["password", "ca", "cert", "key"].map(|name| directory.path().join(name));
        for (target, link) in [password.path(), cert.path(), cert.path(), key.path()]
            .into_iter()
            .zip(&links)
        {
            symlink(target, link).unwrap();
        }
        let options = |[password, ca, cert, key]: [PathBuf; 4]| CacheOptions {
            password_file: Some(password),
            root_ca_path: Some(ca),
            ..with_client_certificate("rediss://127.0.0.1:6379", cert, key)
        };
        Cache::start(options(links.clone()))
            .await
            .expect("regular-file symlinks");

        let fifo = directory.path().join("fifo");
        nix::unistd::mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let kind = ErrorKind::InvalidInput;
        for (index, expected) in [
            CacheError::PasswordFile { kind },
            CacheError::CaFile { kind },
            CacheError::ClientCertificateFile { kind },
            CacheError::ClientKeyFile { kind },
        ]
        .into_iter()
        .enumerate()
        {
            let mut paths = links.clone();
            paths[index].clone_from(&fifo);
            let error = tokio::time::timeout(Duration::from_secs(2), Cache::start(options(paths)))
                .await
                .expect("refuse a FIFO before the admission deadline")
                .unwrap_err();
            assert_eq!(error, expected);
        }
    });
}

#[tokio::test]
async fn a_client_certificate_with_its_key_is_admitted_without_dialing() {
    let (cert, key) = client_identity();
    Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        key.path().to_path_buf(),
    ))
    .await
    .expect("a matching pair is admitted");
}

#[tokio::test]
async fn a_client_certificate_on_plaintext_is_refused_before_the_files_are_read() {
    let err = Cache::start(with_client_certificate(
        "redis://:hunter2@127.0.0.1:6379",
        PathBuf::from("/no/such/client.crt"),
        PathBuf::from("/no/such/client.key"),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::ClientCertificateRequiresTls);
}

#[tokio::test]
async fn a_missing_client_certificate_or_key_file_reports_only_the_io_kind() {
    let (cert, key) = client_identity();
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        PathBuf::from("/no/such/client.crt"),
        key.path().to_path_buf(),
    ))
    .await
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
    .await
    .unwrap_err();
    std::assert_matches!(err, CacheError::ClientKeyFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert_no_leak(&err, &["/no/such"]);
}

#[tokio::test]
async fn a_client_certificate_file_without_a_pem_certificate_is_invalid() {
    let (_, key) = client_identity();
    let file = temp_file(b"not a certificate");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        file.path().to_path_buf(),
        key.path().to_path_buf(),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientCertificate);
}

#[tokio::test]
async fn a_client_certificate_in_valid_pem_but_not_x509_blames_the_certificate() {
    let (_, key) = client_identity();
    let truncated = write_pem("CERTIFICATE", b"not a DER certificate");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        truncated.path().to_path_buf(),
        key.path().to_path_buf(),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientCertificate);
}

#[tokio::test]
async fn a_root_ca_in_valid_pem_but_not_x509_is_invalid() {
    let truncated = write_pem("CERTIFICATE", b"not a DER certificate");
    let err = Cache::start(options(
        TLS_DSN,
        false,
        false,
        Some(truncated.path().to_path_buf()),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
}

#[tokio::test]
async fn a_client_key_that_is_unusable_or_belongs_to_another_certificate_is_invalid() {
    let (cert, _) = client_identity();
    let (_, other_key) = client_identity();
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        other_key.path().to_path_buf(),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);

    let garbage = temp_file(b"not a key");
    let err = Cache::start(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        garbage.path().to_path_buf(),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);
    assert_no_leak(&err, &["hunter2"]);
}

#[tokio::test]
async fn invalid_dsns_fail_before_setup_without_echoing_the_password() {
    for dsn in ["not a url hunter2", "rediss://:hunter2@localhost/-1"] {
        let err = Cache::start(options(dsn, true, true, None))
            .await
            .unwrap_err();
        assert_eq!(err, CacheError::InvalidDsn);
        assert_no_leak(&err, &["hunter2"]);
    }
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

#[tokio::test]
async fn a_password_file_admits_a_dsn_without_a_password() {
    let file = temp_file("hunter2\n");
    Cache::start(with_password_file(
        "redis://127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .await
    .expect("the file is the password source");
}

#[tokio::test]
async fn a_password_in_the_dsn_and_a_password_file_are_refused_together() {
    let file = temp_file("from-file");
    let err = Cache::start(with_password_file(
        "redis://:hunter2@127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordInDsn);
    assert_no_leak(&err, &["hunter2"]);
}

#[tokio::test]
async fn an_unreadable_or_empty_password_file_fails_admission() {
    let missing = PathBuf::from("/no/such/cache-password");
    let err = Cache::start(with_password_file("redis://127.0.0.1:6379", missing))
        .await
        .unwrap_err();
    std::assert_matches!(err, CacheError::PasswordFile { kind } if kind == std::io::ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(!err.to_string().contains("/no/such"), "{err}");

    let file = temp_file("\n");
    let err = Cache::start(with_password_file(
        "redis://127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .await
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordFileEmpty);
}

#[tokio::test]
async fn credential_files_over_their_limit_fail_admission() {
    use std::io::ErrorKind::InvalidData;

    let password = temp_file(vec![b'a'; 16 * 1024]);
    Cache::start(with_password_file(
        "redis://localhost",
        password.path().to_owned(),
    ))
    .await
    .unwrap();
    let oversized = temp_file(vec![b'a'; 16 * 1024 + 1]);
    assert_eq!(
        Cache::start(with_password_file(
            "redis://localhost",
            oversized.path().to_owned()
        ))
        .await
        .unwrap_err(),
        CacheError::PasswordFile { kind: InvalidData }
    );

    let roots = temp_file(vec![b'a'; 1024 * 1024 + 1]);
    let mut with_roots = super::options(TLS_DSN, false, false, None);
    with_roots.root_ca_path = Some(roots.path().to_owned());
    assert_eq!(
        Cache::start(with_roots).await.unwrap_err(),
        CacheError::CaFile { kind: InvalidData }
    );
}
