use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use secrecy::SecretString;
use tls_test_support::TlsMaterial;

use super::base_options;
use crate::{
    ClickHouse, ClickHouseAuth, ClickHouseOptions, ClientCertificate, CloseOutcome, Param,
    QuerySettings, StartError, TransportSecurity,
};

fn memory_settings(bytes: u64) -> QuerySettings {
    QuerySettings::Client {
        max_memory_usage: Some(NonZeroU64::new(bytes).expect("nz")),
    }
}

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
        opts.transport = TransportSecurity::default();
        let err = ClickHouse::start(opts)
            .await
            .expect_err("bad URL must be rejected");
        assert!(matches!(err, StartError(_)), "{bad_url}");
    }

    let plaintext_https = base_options("https://localhost:8443".to_owned());
    assert!(ClickHouse::start(plaintext_https).await.is_err());

    for bad_db in ["", "   ", "app\n", "app\x7f"] {
        let mut opts = base_options("https://localhost:8443".to_owned());
        opts.transport = TransportSecurity::default();
        opts.database = bad_db.to_owned();
        assert!(ClickHouse::start(opts).await.is_err(), "{bad_db:?}");
    }

    let limit_failures: Vec<fn(&mut ClickHouseOptions)> = vec![
        |o| o.max_read_concurrency = NonZeroUsize::new(1025).expect("nz"),
        |o| o.max_write_concurrency = NonZeroUsize::new(1025).expect("nz"),
        |o| o.query_timeout = Duration::from_millis(9),
        |o| o.query_timeout = Duration::from_mins(5) + Duration::from_millis(1),
        |o| o.insert_timeout = Duration::from_millis(9),
        |o| o.insert_timeout = Duration::from_mins(5) + Duration::from_millis(1),
        |o| o.max_result_rows = NonZeroU64::new(10_000_001).expect("nz"),
        |o| o.max_result_bytes = NonZeroUsize::new(64 * 1024 - 1).expect("nz"),
        |o| o.max_result_bytes = NonZeroUsize::new(128 * 1024 * 1024 + 1).expect("nz"),
        |o| o.http_response_buffer_size = NonZeroUsize::new(64 * 1024 - 1).expect("nz"),
        |o| {
            o.max_result_bytes = NonZeroUsize::new(128 * 1024).expect("nz");
            o.http_response_buffer_size = NonZeroUsize::new(256 * 1024).expect("nz");
        },
        |o| o.query_settings = memory_settings(16 * 1024 * 1024 - 1),
        |o| o.query_settings = memory_settings(64 * 1024 * 1024 * 1024 + 1),
    ];

    for mutate in limit_failures {
        let mut opts = base_options("http://127.0.0.1:8123".to_owned());
        mutate(&mut opts);
        assert!(ClickHouse::start(opts).await.is_err());
    }

    let mut min_boundary = base_options("https://localhost:8443/prefix/".to_owned());
    min_boundary.transport = TransportSecurity::default();
    min_boundary.query_settings = QuerySettings::default();
    min_boundary.query_timeout = Duration::from_millis(10);
    min_boundary.insert_timeout = Duration::from_mins(5);
    min_boundary.max_read_concurrency = NonZeroUsize::MIN;
    min_boundary.max_write_concurrency = NonZeroUsize::MIN;
    min_boundary.max_result_rows = NonZeroU64::MIN;
    min_boundary.max_result_bytes = NonZeroUsize::new(64 * 1024).expect("nz");
    min_boundary.http_response_buffer_size = NonZeroUsize::new(64 * 1024).expect("nz");
    let ch = ClickHouse::start(min_boundary).await.expect("boundary ok");
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let tls = TransportSecurity::default();
    let root_norm =
        crate::admission::validate_url("https://127.0.0.1:8443/", &tls).expect("root url ok");
    assert_eq!(root_norm.normalized, "https://127.0.0.1:8443");
    assert_eq!(root_norm.host, "127.0.0.1");
    assert_eq!(root_norm.port, 8443);

    let prefix_norm = crate::admission::validate_url("https://127.0.0.1:8443/prefix/", &tls)
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

    let mut unauth_ok = base_options("http://127.0.0.1:8123".to_owned());
    unauth_ok.auth = ClickHouseAuth::Unauthenticated { username: None };
    let ch_unauth = ClickHouse::start(unauth_ok).await.expect("unauth default");
    assert_eq!(
        ch_unauth.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let mut pw_file_ok = base_options("http://127.0.0.1:8123".to_owned());
    pw_file_ok.auth = ClickHouseAuth::PasswordFile {
        username: "app".to_owned(),
        path: pw_path.clone(),
    };
    let ch_file = ClickHouse::start(pw_file_ok)
        .await
        .expect("valid password_file admitted");
    assert_eq!(
        ch_file.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    for bad_auth in [
        ClickHouseAuth::Password {
            username: "app".to_owned(),
            password: SecretString::from("   \t  "),
        },
        ClickHouseAuth::Password {
            username: "app".to_owned(),
            password: SecretString::from("bad\nsecret"),
        },
        ClickHouseAuth::Password {
            username: String::new(),
            password: SecretString::from("app"),
        },
        ClickHouseAuth::Password {
            username: "bad\ruser".to_owned(),
            password: SecretString::from("app"),
        },
        ClickHouseAuth::PasswordFile {
            username: String::new(),
            path: pw_path.clone(),
        },
        ClickHouseAuth::PasswordFile {
            username: "bad\ruser".to_owned(),
            path: pw_path,
        },
        ClickHouseAuth::Unauthenticated {
            username: Some("bad\ruser".to_owned()),
        },
    ] {
        let mut opts = base_options("http://127.0.0.1:8123".to_owned());
        opts.auth = bad_auth;
        assert!(ClickHouse::start(opts).await.is_err());
    }

    assert_invalid_password_files(dir.path()).await;

    let secret = "super-secret-password-value";
    let mut redact_opts = base_options("https://localhost:8443".to_owned());
    redact_opts.transport = TransportSecurity::default();
    redact_opts.auth = ClickHouseAuth::Password {
        username: "app".to_owned(),
        password: SecretString::from(secret),
    };
    assert_secret_redaction(redact_opts, secret).await;
}

async fn assert_invalid_password_files(dir: &std::path::Path) {
    for bad_file_bytes in [
        b"".as_slice(),
        b"\r\n\n".as_slice(),
        b"line1\nline2\n".as_slice(),
        b"has\0null".as_slice(),
        b"has\x7fdel".as_slice(),
        &[0xff, 0xfe],
        &vec![b'a'; 16 * 1024 + 1],
    ] {
        let bad_path = dir.join("bad_pw");
        tokio::fs::write(&bad_path, bad_file_bytes)
            .await
            .expect("write bad pw");
        let mut opts = base_options("http://127.0.0.1:8123".to_owned());
        opts.auth = ClickHouseAuth::PasswordFile {
            username: "app".to_owned(),
            path: bad_path,
        };
        assert!(
            ClickHouse::start(opts).await.is_err(),
            "bad password_file len={} must fail",
            bad_file_bytes.len()
        );
    }

    let mut dir_pw = base_options("http://127.0.0.1:8123".to_owned());
    dir_pw.auth = ClickHouseAuth::PasswordFile {
        username: "app".to_owned(),
        path: dir.to_path_buf(),
    };
    assert!(ClickHouse::start(dir_pw).await.is_err());

    #[cfg(unix)]
    {
        let fifo_path = dir.join("pw.fifo");
        nix::unistd::mkfifo(
            &fifo_path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .expect("mkfifo");
        let mut fifo_opts = base_options("http://127.0.0.1:8123".to_owned());
        fifo_opts.auth = ClickHouseAuth::PasswordFile {
            username: "app".to_owned(),
            path: fifo_path,
        };
        let res = tokio::time::timeout(Duration::from_secs(2), ClickHouse::start(fifo_opts))
            .await
            .expect("FIFO open must not hang");
        assert!(res.is_err(), "FIFO password_file must fail immediately");
    }
}

async fn assert_secret_redaction(opts: ClickHouseOptions, secret: &str) {
    let opts_dbg = format!("{opts:?}");
    assert!(!opts_dbg.contains(secret), "{opts_dbg}");

    let param_secret = Param::from("secret-param-123");
    let param_dbg = format!("{param_secret:?}");
    assert!(!param_dbg.contains("secret-param-123"), "{param_dbg}");

    let ch = ClickHouse::start(opts).await.expect("start");
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

    let mut bad_ca = base_options("https://localhost:8443".to_owned());
    bad_ca.transport = TransportSecurity::Tls {
        root_ca_path: Some(bad_pem),
        client_certificate: None,
    };
    assert!(ClickHouse::start(bad_ca).await.is_err());

    let mut oversized_ca = base_options("https://localhost:8443".to_owned());
    oversized_ca.transport = TransportSecurity::Tls {
        root_ca_path: Some(huge_pem),
        client_certificate: None,
    };
    assert!(ClickHouse::start(oversized_ca).await.is_err());

    let mut oversized_key = base_options("https://localhost:8443".to_owned());
    oversized_key.transport = TransportSecurity::Tls {
        root_ca_path: None,
        client_certificate: Some(ClientCertificate {
            cert_path: cert_path.clone(),
            key_path: huge_key,
        }),
    };
    assert!(ClickHouse::start(oversized_key).await.is_err());

    let mut mismatched = base_options("https://localhost:8443".to_owned());
    mismatched.transport = TransportSecurity::Tls {
        root_ca_path: Some(ca_path.clone()),
        client_certificate: Some(ClientCertificate {
            cert_path: cert_path.clone(),
            key_path: key_b_path,
        }),
    };
    assert!(ClickHouse::start(mismatched).await.is_err());

    let mut valid = base_options("https://localhost:8443".to_owned());
    valid.transport = TransportSecurity::Tls {
        root_ca_path: Some(ca_path),
        client_certificate: Some(ClientCertificate {
            cert_path,
            key_path: key_a_path,
        }),
    };
    let ch = ClickHouse::start(valid).await.expect("valid mtls");
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}
