use std::time::Duration;

use secrecy::SecretString;
use tls_test_support::TlsMaterial;

use super::base_options;
use crate::{ClickHouse, ClickHouseOptions, ClientCertificate, CloseOutcome, Param, StartError};

#[tokio::test]
async fn url_database_and_limits_admission_boundaries() {
    for bad_url in [
        "",
        "ftp://127.0.0.1:8123",
        "http://127.0.0.1:8123",
        "https://user:pass@127.0.0.1:8123",
        "https://user@127.0.0.1:8123",
        "https://@127.0.0.1:8123",
        "https://127.0.0.1:8123?database=app",
        "https://127.0.0.1:8123#frag",
        "https://127.0.0.1:8123/a/./b",
        "https://127.0.0.1:8123/a/../b",
        "https://127.0.0.1:8123/a/%2e%2e/b",
        "https://127.0.0.1:8123/a/.%2e/b",
        "https://127.0.0.1:8123/a/%2e./b",
        "https://127.0.0.1:8123/a\\..\\b",
        "https:///127.0.0.1:8443",
        "https:/127.0.0.1:8443",
        "https://127.0.0.1:8123/a//b",
        "https://127.0.0.1:8123//",
        " https://127.0.0.1:8123",
        "https://127.0.0.1:8123/\n",
        "https://",
    ] {
        let mut opts = base_options(bad_url.to_owned());
        opts.allow_plaintext = false;
        let err = ClickHouse::start(opts)
            .await
            .expect_err("bad URL must be rejected");
        assert!(matches!(err, StartError(_)), "{bad_url}");
    }

    for bad_db in ["", "   ", "app\n", "app\x7f"] {
        let mut opts = base_options("https://localhost:8443".to_owned());
        opts.allow_plaintext = false;
        opts.database = bad_db.to_owned();
        assert!(ClickHouse::start(opts).await.is_err(), "{bad_db:?}");
    }

    let limit_failures: Vec<fn(&mut ClickHouseOptions)> = vec![
        |o| o.max_read_concurrency = 0,
        |o| o.max_read_concurrency = 1025,
        |o| o.max_write_concurrency = 0,
        |o| o.max_write_concurrency = 1025,
        |o| o.query_timeout = Duration::from_millis(9),
        |o| o.query_timeout = Duration::from_mins(5) + Duration::from_millis(1),
        |o| o.insert_timeout = Duration::from_millis(9),
        |o| o.insert_timeout = Duration::from_mins(5) + Duration::from_millis(1),
        |o| o.max_result_rows = 0,
        |o| o.max_result_rows = 10_000_001,
        |o| o.max_result_bytes = 64 * 1024 - 1,
        |o| o.max_result_bytes = 128 * 1024 * 1024 + 1,
        |o| o.http_response_buffer_size = 64 * 1024 - 1,
        |o| {
            o.max_result_bytes = 128 * 1024;
            o.http_response_buffer_size = 256 * 1024;
        },
        |o| o.max_memory_usage = Some(0),
        |o| o.max_memory_usage = Some(16 * 1024 * 1024 - 1),
        |o| o.max_memory_usage = Some(64 * 1024 * 1024 * 1024 + 1),
    ];

    for mutate in limit_failures {
        let mut opts = base_options("http://127.0.0.1:8123".to_owned());
        mutate(&mut opts);
        assert!(ClickHouse::start(opts).await.is_err());
    }

    let mut min_boundary = base_options("https://localhost:8443/prefix/".to_owned());
    min_boundary.allow_plaintext = false;
    min_boundary.query_timeout = Duration::from_millis(10);
    min_boundary.insert_timeout = Duration::from_mins(5);
    min_boundary.max_read_concurrency = 1;
    min_boundary.max_write_concurrency = 1;
    min_boundary.max_result_rows = 1;
    min_boundary.max_result_bytes = 64 * 1024;
    min_boundary.http_response_buffer_size = 64 * 1024;
    min_boundary.max_memory_usage = None;
    let ch = ClickHouse::start(min_boundary).await.expect("boundary ok");
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let root_norm =
        crate::admission::validate_url("https://127.0.0.1:8443/", false).expect("root url ok");
    assert_eq!(root_norm.normalized, "https://127.0.0.1:8443");
    assert_eq!(root_norm.host, "127.0.0.1");
    assert_eq!(root_norm.port, 8443);
    assert!(root_norm.is_https);

    let prefix_norm = crate::admission::validate_url("https://127.0.0.1:8443/prefix/", false)
        .expect("prefix url ok");
    assert_eq!(prefix_norm.normalized, "https://127.0.0.1:8443/prefix");

    let ch_root = ClickHouse::start(base_options("http://127.0.0.1:8123/".to_owned()))
        .await
        .expect("root slash start ok");
    assert_eq!(
        ch_root.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn credential_matrix_secret_redaction_and_password_file_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pw_path = dir.path().join("pw");
    tokio::fs::write(&pw_path, " secret-with-spaces \r\n")
        .await
        .expect("write pw");

    let mut unauth_with_pw = base_options("http://127.0.0.1:8123".to_owned());
    unauth_with_pw.allow_unauthenticated = true;
    assert!(ClickHouse::start(unauth_with_pw).await.is_err());

    let mut unauth_with_file = base_options("http://127.0.0.1:8123".to_owned());
    unauth_with_file.allow_unauthenticated = true;
    unauth_with_file.password = None;
    unauth_with_file.password_file = Some(pw_path.clone());
    assert!(ClickHouse::start(unauth_with_file).await.is_err());

    let mut unauth_ok = base_options("http://127.0.0.1:8123".to_owned());
    unauth_ok.allow_unauthenticated = true;
    unauth_ok.username = None;
    unauth_ok.password = None;
    let ch_unauth = ClickHouse::start(unauth_ok).await.expect("unauth default");
    assert_eq!(
        ch_unauth.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let mut conflict = base_options("http://127.0.0.1:8123".to_owned());
    conflict.password_file = Some(pw_path.clone());
    assert!(ClickHouse::start(conflict).await.is_err());

    let mut blank_inline_with_file = base_options("http://127.0.0.1:8123".to_owned());
    blank_inline_with_file.password = Some(SecretString::from("   \t  "));
    blank_inline_with_file.password_file = Some(pw_path.clone());
    let ch_file = ClickHouse::start(blank_inline_with_file)
        .await
        .expect("blank inline password treated as unset");
    assert_eq!(
        ch_file.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let mut ctl_inline = base_options("http://127.0.0.1:8123".to_owned());
    ctl_inline.password = Some(SecretString::from("bad\nsecret"));
    assert!(ClickHouse::start(ctl_inline).await.is_err());

    let mut ctl_user = base_options("http://127.0.0.1:8123".to_owned());
    ctl_user.username = Some("bad\ruser".to_owned());
    assert!(ClickHouse::start(ctl_user).await.is_err());

    for bad_file_bytes in [
        b"".as_slice(),
        b"\r\n\n".as_slice(),
        b"line1\nline2\n".as_slice(),
        b"has\0null".as_slice(),
        b"has\x7fdel".as_slice(),
        &[0xff, 0xfe],
        &vec![b'a'; 16 * 1024 + 1],
    ] {
        let bad_path = dir.path().join("bad_pw");
        tokio::fs::write(&bad_path, bad_file_bytes)
            .await
            .expect("write bad pw");
        let mut opts = base_options("http://127.0.0.1:8123".to_owned());
        opts.password = None;
        opts.password_file = Some(bad_path);
        assert!(
            ClickHouse::start(opts).await.is_err(),
            "bad password_file len={} must fail",
            bad_file_bytes.len()
        );
    }

    let mut dir_pw = base_options("http://127.0.0.1:8123".to_owned());
    dir_pw.password = None;
    dir_pw.password_file = Some(dir.path().to_path_buf());
    assert!(ClickHouse::start(dir_pw).await.is_err());

    #[cfg(unix)]
    {
        let fifo_path = dir.path().join("pw.fifo");
        nix::unistd::mkfifo(
            &fifo_path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .expect("mkfifo");
        let mut fifo_opts = base_options("http://127.0.0.1:8123".to_owned());
        fifo_opts.password = None;
        fifo_opts.password_file = Some(fifo_path);
        let res = tokio::time::timeout(Duration::from_secs(2), ClickHouse::start(fifo_opts))
            .await
            .expect("FIFO open must not hang");
        assert!(res.is_err(), "FIFO password_file must fail immediately");
    }

    let secret = "super-secret-password-value";
    let mut redact_opts = base_options("https://localhost:8443".to_owned());
    redact_opts.allow_plaintext = false;
    redact_opts.password = Some(SecretString::from(secret));
    let opts_dbg = format!("{redact_opts:?}");
    assert!(!opts_dbg.contains(secret), "{opts_dbg}");

    let param_secret = Param::from("secret-param-123");
    let param_dbg = format!("{param_secret:?}");
    assert!(!param_dbg.contains("secret-param-123"), "{param_dbg}");

    let ch = ClickHouse::start(redact_opts).await.expect("start");
    let ch_dbg = format!("{ch:?}");
    assert!(!ch_dbg.contains(secret), "{ch_dbg}");
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn tls_and_mtls_admission_rejects_invalid_and_accepts_valid_pems() {
    let tls_a = TlsMaterial::new("localhost");
    let tls_b = TlsMaterial::new("localhost");
    let dir = tempfile::tempdir().expect("tempdir");

    let ca_path = dir.path().join("ca.pem");
    let cert_path = dir.path().join("client.pem");
    let key_a_path = dir.path().join("client_a.key");
    let key_b_path = dir.path().join("client_b.key");
    let bad_pem = dir.path().join("bad.pem");
    let huge_pem = dir.path().join("huge.pem");
    let huge_key = dir.path().join("huge.key");

    tokio::fs::write(&ca_path, &tls_a.root_pem)
        .await
        .expect("ca");
    tokio::fs::write(&cert_path, &tls_a.cert_pem)
        .await
        .expect("cert");
    tokio::fs::write(&key_a_path, &tls_a.key_pem)
        .await
        .expect("key a");
    tokio::fs::write(&key_b_path, &tls_b.key_pem)
        .await
        .expect("key b");
    tokio::fs::write(&bad_pem, "not a pem")
        .await
        .expect("bad pem");
    tokio::fs::write(&huge_pem, vec![b'x'; 1024 * 1024 + 1])
        .await
        .expect("huge pem");
    tokio::fs::write(&huge_key, vec![b'x'; 64 * 1024 + 1])
        .await
        .expect("huge key");

    let mut http_with_ca = base_options("http://127.0.0.1:8123".to_owned());
    http_with_ca.root_ca_path = Some(ca_path.clone());
    assert!(ClickHouse::start(http_with_ca).await.is_err());

    let mut http_with_cert = base_options("http://127.0.0.1:8123".to_owned());
    http_with_cert.client_certificate = Some(ClientCertificate {
        cert_path: cert_path.clone(),
        key_path: key_a_path.clone(),
    });
    assert!(ClickHouse::start(http_with_cert).await.is_err());

    let mut bad_ca = base_options("https://localhost:8443".to_owned());
    bad_ca.allow_plaintext = false;
    bad_ca.root_ca_path = Some(bad_pem.clone());
    assert!(ClickHouse::start(bad_ca).await.is_err());

    let mut oversized_ca = base_options("https://localhost:8443".to_owned());
    oversized_ca.allow_plaintext = false;
    oversized_ca.root_ca_path = Some(huge_pem);
    assert!(ClickHouse::start(oversized_ca).await.is_err());

    let mut oversized_key = base_options("https://localhost:8443".to_owned());
    oversized_key.allow_plaintext = false;
    oversized_key.client_certificate = Some(ClientCertificate {
        cert_path: cert_path.clone(),
        key_path: huge_key,
    });
    assert!(ClickHouse::start(oversized_key).await.is_err());

    let mut mismatched = base_options("https://localhost:8443".to_owned());
    mismatched.allow_plaintext = false;
    mismatched.root_ca_path = Some(ca_path.clone());
    mismatched.client_certificate = Some(ClientCertificate {
        cert_path: cert_path.clone(),
        key_path: key_b_path,
    });
    assert!(ClickHouse::start(mismatched).await.is_err());

    let mut valid = base_options("https://localhost:8443".to_owned());
    valid.allow_plaintext = false;
    valid.root_ca_path = Some(ca_path);
    valid.client_certificate = Some(ClientCertificate {
        cert_path,
        key_path: key_a_path,
    });
    let ch = ClickHouse::start(valid).await.expect("valid mtls");
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}
