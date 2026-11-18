use super::super::tests::{env, options, tracer_provider};
use super::*;

#[test]
fn collector_paths_follow_the_selected_endpoint_source() {
    let root = (OTEL_EXPORTER_OTLP_ENDPOINT, "https://collector/base/");
    let signal = (
        OTEL_EXPORTER_OTLP_TRACES_ENDPOINT,
        "https://collector/custom",
    );
    for (typed, pairs, expected) in [
        ("", &[][..], None),
        ("  ", &[root][..], Some("https://collector/base/v1/traces")),
        (
            "",
            &[root, (signal.0, " ")][..],
            Some("https://collector/base/v1/traces"),
        ),
        ("", &[root, signal][..], Some(signal.1)),
        (
            "",
            &[(signal.0, "https://collector")][..],
            Some("https://collector/"),
        ),
        (
            "http://typed",
            &[root, signal][..],
            Some("http://typed/v1/traces"),
        ),
        (
            "https://typed/custom",
            &[][..],
            Some("https://typed/custom"),
        ),
    ] {
        let resolved =
            CollectorConfig::resolve(&options(typed), &CollectorTrust::default(), &env(pairs))
                .unwrap();
        assert_eq!(resolved.as_ref().map(|c| c.url.as_str()), expected);
    }
}

#[test]
fn credentials_require_a_matching_explicit_https_collector() {
    let standard = "https://collector";
    let credential = "authorization=private-value";
    let traces_headers = OTEL_EXPORTER_OTLP_TRACES_HEADERS;
    let general_headers = OTEL_EXPORTER_OTLP_HEADERS;
    let root = OTEL_EXPORTER_OTLP_ENDPOINT;
    let signal = OTEL_EXPORTER_OTLP_TRACES_ENDPOINT;
    for (typed, typed_headers, pairs, name) in [
        (
            standard,
            None,
            &[(general_headers, credential)][..],
            general_headers,
        ),
        (
            standard,
            None,
            &[(traces_headers, " "), (general_headers, credential)][..],
            general_headers,
        ),
        ("", Some(credential), &[(root, standard)][..], TYPED_HEADERS),
        ("", Some(credential), &[][..], TYPED_HEADERS),
        (
            "",
            None,
            &[(general_headers, credential)][..],
            general_headers,
        ),
        ("http://collector", Some(credential), &[][..], TYPED_HEADERS),
        (
            "",
            None,
            &[(root, "http://collector"), (general_headers, credential)][..],
            general_headers,
        ),
        (
            "",
            None,
            &[
                (root, standard),
                (signal, "https://other/v1/traces"),
                (general_headers, credential),
            ][..],
            general_headers,
        ),
    ] {
        let mut opts = options(typed);
        opts.otlp_headers = typed_headers.map(secrecy::SecretString::from);
        let error = tracer_provider_for_test(&opts, pairs)
            .unwrap_err()
            .to_string();
        assert!(error.contains(name), "{error}");
        assert!(!error.contains("private-value"));
    }
}

fn tracer_provider_for_test(
    options: &TracingOptions,
    pairs: &[(&str, &str)],
) -> Result<super::super::TracerProviderHandle, TracingError> {
    tracer_provider(options, env(pairs), &CollectorTrust::from_env(env(pairs)))
}

#[test]
fn typed_collector_requires_each_client_identity_file_to_have_a_matching_standard_destination() {
    let options = options("https://collector");
    let identity = "/private-identity-file";
    let common = (OTEL_EXPORTER_OTLP_ENDPOINT, "https://other");
    let signal = (
        OTEL_EXPORTER_OTLP_TRACES_ENDPOINT,
        "https://collector/v1/traces",
    );
    for (pairs, expected) in [
        (
            &[(CLIENT_CERTIFICATE_VARS[0], identity)][..],
            CLIENT_CERTIFICATE_VARS[0],
        ),
        (&[(CLIENT_KEY_VARS[1], identity)][..], CLIENT_KEY_VARS[1]),
        (
            &[common, (CLIENT_CERTIFICATE_VARS[0], identity)][..],
            CLIENT_CERTIFICATE_VARS[0],
        ),
        (
            &[
                common,
                signal,
                (CLIENT_CERTIFICATE_VARS[0], identity),
                (CLIENT_KEY_VARS[1], identity),
            ][..],
            CLIENT_KEY_VARS[1],
        ),
        (
            &[
                (OTEL_EXPORTER_OTLP_ENDPOINT, "https://collector"),
                (OTEL_EXPORTER_OTLP_TRACES_ENDPOINT, "https://other/traces"),
                (CLIENT_KEY_VARS[1], identity),
            ][..],
            CLIENT_KEY_VARS[1],
        ),
    ] {
        let error = tracer_provider_for_test(&options, pairs).unwrap_err();
        std::assert_matches!(error, TracingError::UnboundCredential { name } if name == expected);
        assert!(!error.to_string().contains(identity));
    }
}

#[test]
fn both_endpoint_sources_reject_malformed_or_secret_urls_without_echoing_them() {
    for raw in [
        "https://collector/path?private-marker",
        "https://private-marker@collector",
        "https://@collector/private-marker",
        "https://collector/private-marker#fragment",
        "https:///private-marker",
        "https://collector:invalid/private-marker",
        "https://collector\\other/private-marker",
        "collector",
    ] {
        for (typed, pairs) in [
            (raw, &[][..]),
            ("", &[(OTEL_EXPORTER_OTLP_ENDPOINT, raw)][..]),
        ] {
            let error = tracer_provider_for_test(&options(typed), pairs).unwrap_err();
            std::assert_matches!(error, TracingError::InvalidEndpoint { .. });
            assert!(!format!("{error:?} {error}").contains("private-marker"));
        }
    }
}

#[test]
fn blank_signal_headers_fall_back_and_occupied_signal_headers_override() {
    for (signal, expected) in [
        ("", "general"),
        ("  ", "general"),
        ("AUTHORIZATION=signal", "signal"),
    ] {
        let pairs = [
            (OTEL_EXPORTER_OTLP_ENDPOINT, "https://collector"),
            (OTEL_EXPORTER_OTLP_HEADERS, "Authorization=general"),
            (OTEL_EXPORTER_OTLP_TRACES_HEADERS, signal),
        ];
        let collector =
            CollectorConfig::resolve(&options(""), &CollectorTrust::default(), &env(&pairs))
                .unwrap()
                .unwrap();
        assert_eq!(
            collector.headers.get("authorization").map(String::as_str),
            Some(expected)
        );
    }
}

#[test]
fn trust_files_come_from_the_standard_variables_traces_variant_first() {
    let trust = CollectorTrust::from_env(|name| match name {
        "OTEL_EXPORTER_OTLP_CERTIFICATE" => Some("/general/ca.pem".into()),
        "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE" => Some("/traces/ca.pem".into()),
        "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE" => Some("  ".into()),
        "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE" => Some("/general/client.pem".into()),
        _ => None,
    });
    assert_eq!(
        trust,
        CollectorTrust {
            certificate: Some(TrustFile {
                variable: "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
                path: "/traces/ca.pem".into(),
            }),
            client_certificate: Some(TrustFile {
                variable: "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
                path: "/general/client.pem".into(),
            }),
            client_key: None,
        }
    );
    assert_eq!(
        CollectorTrust::from_env(|_| None),
        CollectorTrust::default()
    );
}

#[test]
fn header_pairs_parse_and_blank_entries_are_skipped() {
    let headers = parse_headers(TYPED_HEADERS, "authorization=Basic%20x, x-tenant = t1 ,").unwrap();
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Basic%20x")
    );
    assert_eq!(headers.get("x-tenant").map(String::as_str), Some("t1"));
    assert_eq!(headers.len(), 2);
}

#[test]
fn malformed_header_entries_fail_by_position() {
    for (raw, position) in [
        ("authorization: Bearer x", 1),
        ("a=b,=novalue", 2),
        ("AUTHORIZATION=first,Authorization=second", 2),
        ("Host=another-collector", 1),
        ("a=b,,bad name=x", 3),
        ("a=line\nbreak", 1),
        ("a=b,,auth=private%0A,auth=replacement", 3),
        ("auth=private%+A", 1),
        ("auth=private%00", 1),
    ] {
        let err = parse_headers(TYPED_HEADERS, raw).unwrap_err();
        std::assert_matches!(err, TracingError::InvalidHeader { position: p, .. } if p == position,
            "{raw:?}: {err}"
        );
    }
}

#[test]
fn https_otlp_endpoint_is_admitted_by_the_exporter_client() {
    use futures_util::FutureExt;

    let exporter = span_exporter(
        "https://127.0.0.1:1/v1/traces",
        HashMap::new(),
        &CollectorTrust::default(),
        export_timeout(&|_: &str| None),
        None,
    )
    .expect("https endpoint must build");
    let result = {
        use opentelemetry_sdk::trace::SpanExporter as _;
        exporter
            .export(Vec::new())
            .now_or_never()
            .expect("blocking OTLP client must finish in one poll")
    };
    let err = result.expect_err("nothing listens on 127.0.0.1:1");
    let message = err.to_string();
    assert!(
        !message.contains("URL scheme is not allowed"),
        "https must be a permitted scheme: {message}"
    );
}

#[test]
fn compression_falls_back_and_remembers_only_ignored_variable_names() {
    let traces = OTEL_EXPORTER_OTLP_TRACES_COMPRESSION;
    let general = OTEL_EXPORTER_OTLP_COMPRESSION;
    let gzip = Some(opentelemetry_otlp::Compression::Gzip);
    for (pairs, expected, ignored) in [
        (&[][..], None, vec![]),
        (&[(general, "GZIP")][..], gzip, vec![]),
        (&[(traces, "none"), (general, "gzip")][..], None, vec![]),
        (&[(traces, ""), (general, "gzip")][..], gzip, vec![]),
        (
            &[(traces, "unknown"), (general, "gzip")][..],
            gzip,
            vec![traces],
        ),
        (
            &[(traces, "unknown"), (general, "zstd")][..],
            None,
            vec![traces, general],
        ),
    ] {
        assert_eq!(compression(&env(pairs)), (expected, ignored));
    }
}
