use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

use super::*;

fn tls_collector(config: Arc<ServerConfig>) -> Collector {
    Collector::start(move |stream| {
        let tls = ServerConnection::new(Arc::clone(&config)).ok()?;
        exchange(StreamOwned::new(tls, stream))
    })
}

struct Pki {
    issuer: CertifiedIssuer<'static, KeyPair>,
    files: tempfile::TempDir,
}

impl Pki {
    fn new() -> Self {
        let mut ca = CertificateParams::default();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        Self {
            issuer: CertifiedIssuer::self_signed(ca, KeyPair::generate().expect("CA key"))
                .expect("self-signed CA"),
            files: tempfile::tempdir().expect("directory for PEM files"),
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    fn file(&self, scope: EnvScope, variable: &'static str, name: &str, pem: &str) -> TrustFile {
        let path = self.files.path().join(name);
        std::fs::write(&path, pem).expect("write a PEM file");
        TrustFile {
            scope,
            name: variable,
            path,
        }
    }

    fn certificate_file(&self) -> TrustFile {
        self.file(
            EnvScope::Common,
            CERTIFICATE_VARS[1].1,
            "ca.pem",
            &self.issuer.pem(),
        )
    }

    fn client_files(&self, bundled: bool) -> (TrustFile, TrustFile) {
        let key = KeyPair::generate().expect("client key");
        let mut params = CertificateParams::new(vec!["exporter".to_owned()]).expect("client names");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let certificate = params.signed_by(&key, &self.issuer).expect("client leaf");
        if bundled {
            let certificate = self.file(
                EnvScope::Common,
                CLIENT_CERTIFICATE_VARS[1].1,
                "client-bundle.pem",
                &format!("{}\n{}", certificate.pem(), key.serialize_pem()),
            );
            #[cfg(unix)]
            let certificate = {
                let path = self.files.path().join("mounted-client.pem");
                std::os::unix::fs::symlink(&certificate.path, &path).expect("link to the bundle");
                TrustFile {
                    path,
                    ..certificate
                }
            };
            let key = TrustFile {
                scope: EnvScope::Common,
                name: CLIENT_KEY_VARS[1].1,
                path: certificate.path.clone(),
            };
            return (certificate, key);
        }
        (
            self.file(
                EnvScope::Common,
                CLIENT_CERTIFICATE_VARS[1].1,
                "client.pem",
                &certificate.pem(),
            ),
            self.file(
                EnvScope::Common,
                CLIENT_KEY_VARS[1].1,
                "client.key",
                &key.serialize_pem(),
            ),
        )
    }

    fn collector(&self, require_client_certificate: bool) -> Arc<ServerConfig> {
        let key = KeyPair::generate().expect("collector key");
        let mut params =
            CertificateParams::new(vec!["127.0.0.1".to_owned()]).expect("collector names");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params
            .signed_by(&key, &self.issuer)
            .expect("collector leaf");
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("protocol versions");
        let builder = if require_client_certificate {
            let mut roots = RootCertStore::empty();
            roots.add(self.issuer.der().clone()).expect("CA root");
            builder.with_client_cert_verifier(
                WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                    .build()
                    .expect("client verifier"),
            )
        } else {
            builder.with_no_client_auth()
        };
        Arc::new(
            builder
                .with_single_cert(
                    vec![certificate.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("collector certificate"),
        )
    }
}

fn export(collector: &Collector, trust: &CollectorTrust) -> OTelSdkResult {
    build_span_exporter(
        &format!("https://{}/v1/traces", collector.addr),
        HashMap::new(),
        trust,
        export_timeout_from_env(&|_: &str| None),
        None,
    )
    .expect("the exporter builds")
    .export(Vec::new())
    .now_or_never()
    .expect("the blocking client finishes in one poll")
}

#[test]
fn a_certificate_file_is_what_makes_a_private_collector_trusted() {
    let pki = Pki::new();
    let collector = tls_collector(pki.collector(false));

    export(&collector, &CollectorTrust::default())
        .expect_err("the platform trust store does not know the private CA");

    let trust = CollectorTrust {
        certificate: Some(pki.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &trust).expect("the certificate file's CA signed the collector");
    assert_eq!(collector.received().path, "/v1/traces");

    let other = Pki::new();
    let stranger = CollectorTrust {
        certificate: Some(other.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &stranger).expect_err("another CA did not sign the collector");
}

#[test]
fn a_client_certificate_is_presented_to_a_collector_that_requires_one() {
    let pki = Pki::new();
    let collector = tls_collector(pki.collector(true));
    let anonymous = CollectorTrust {
        certificate: Some(pki.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &anonymous).expect_err("the collector requires a client certificate");

    let endpoint = format!("https://{}/v1/traces", collector.addr);
    for (typed, bundled) in [("", false), (&endpoint, false), (&endpoint, true)] {
        let (client_certificate, client_key) = pki.client_files(bundled);
        let identified = CollectorTrust {
            client_certificate: Some(client_certificate),
            client_key: Some(client_key),
            ..anonymous.clone()
        };
        let handle = tracer_provider(
            &options(typed),
            env(&[(
                opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT,
                &endpoint,
            )]),
            &identified,
        )
        .expect("the standard destination binds the client identity");
        handle.tracer().start("authenticated export").end();
        assert_eq!(shutdown(handle), ProviderShutdown::Completed);
        let request = collector.received();
        assert_eq!(request.path, "/v1/traces");
        assert_eq!(request.span_names(), ["authenticated export"]);
    }
}

#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the synchronous child process isolates SDK environment and exporter globals"
)]
fn admitted_credentials_reach_only_the_selected_tls_collector() {
    const CHILD: &str = "OTLP_TEST_COLLECTOR_MODE";
    const ENDPOINT: &str = "OTLP_TEST_COLLECTOR_URL";
    const HEADERS: &str =
        "Authorization=Bearer%20fixture,X-Once=%250A,X-Bad-Escape=%0A%ZZ,X-Bad-Utf8=%0A%FF";
    if let Ok(mode) = std::env::var(CHILD) {
        let endpoint = (mode == "typed").then(|| std::env::var(ENDPOINT).unwrap());
        let headers = (mode == "typed").then(|| SecretString::from(HEADERS));
        let mut opts = options("");
        opts.otlp_endpoint = endpoint.as_deref();
        opts.otlp_headers = headers.as_ref();
        let provider = runtime().block_on(install_tracer_provider(&opts)).unwrap();
        provider.tracer().start("admitted span").end();
        assert_eq!(shutdown(provider), ProviderShutdown::Completed);
        return;
    }
    let pki = Pki::new();
    let collector = tls_collector(pki.collector(false));
    let endpoint = format!("https://{}", collector.addr);
    let ca = pki.certificate_file();
    for mode in ["typed", "empty signal headers", "trace headers"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "traces::tests::delivery::tls::admitted_credentials_reach_only_the_selected_tls_collector", "--no-capture"])
            .env_clear()
            .env(CHILD, mode)
            .env(CERTIFICATE_VARS[0].1, &ca.path);
        match mode {
            "typed" => {
                child.env(ENDPOINT, &endpoint);
            }
            "empty signal headers" => {
                child
                    .env("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint)
                    .env("OTEL_EXPORTER_OTLP_HEADERS", HEADERS)
                    .env("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "");
            }
            _ => {
                child
                    .env(
                        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                        format!("{endpoint}/v1/traces"),
                    )
                    .env("OTEL_EXPORTER_OTLP_ENDPOINT", "https://unused.invalid")
                    .env("OTEL_EXPORTER_OTLP_TRACES_HEADERS", HEADERS)
                    .env(
                        "OTEL_EXPORTER_OTLP_HEADERS",
                        "Authorization=other,X-Unused=other",
                    );
            }
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let received = collector.received();
        assert_eq!(received.path, "/v1/traces");
        assert_eq!(received.header("authorization"), Some("Bearer fixture"));
        assert_eq!(received.header("x-once"), Some("%0A"));
        assert_eq!(received.header("x-bad-escape"), Some("%0A%ZZ"));
        assert_eq!(received.header("x-bad-utf8"), Some("%0A%FF"));
        assert_eq!(received.header("x-unused"), None);
        assert_eq!(received.span_names(), ["admitted span"]);
    }
}
