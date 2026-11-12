use std::collections::HashMap;

use super::super::tests::{env, options};
use super::*;

#[test]
fn typed_endpoint_wins_and_refuses_ambient_credentials() {
    let typed = options("http://collector:4318");
    let collector = Some(Endpoint::Typed("http://collector:4318"));
    assert_eq!(resolve_endpoint(&typed, env(&[])).unwrap(), collector);
    for (pairs, refused) in [
        (
            &[(OTEL_EXPORTER_OTLP_HEADERS, "a=b")][..],
            OTEL_EXPORTER_OTLP_HEADERS,
        ),
        (
            &[(OTEL_EXPORTER_OTLP_TRACES_HEADERS, "a=b")][..],
            OTEL_EXPORTER_OTLP_TRACES_HEADERS,
        ),
    ] {
        std::assert_matches!(
                resolve_endpoint(&typed, env(pairs)),
                Err(TracingError::AmbientCredential { name }) if name == refused
            ,
            "{pairs:?}"
        );
    }
    // A set traces header variable, even empty or blank, is the one the
    // SDK reads, so the general one never reaches the typed collector.
    for traces in ["", " "] {
        assert_eq!(
            resolve_endpoint(
                &typed,
                env(&[
                    (OTEL_EXPORTER_OTLP_TRACES_HEADERS, traces),
                    (OTEL_EXPORTER_OTLP_HEADERS, "a=b"),
                ])
            )
            .unwrap(),
            collector
        );
    }
    // Trust variables are not read by the exporter, so they cannot carry
    // a credential to the typed collector.
    assert_eq!(
        resolve_endpoint(
            &typed,
            env(&[("OTEL_EXPORTER_OTLP_CERTIFICATE", "/ca.pem")])
        )
        .unwrap(),
        collector
    );
    // Ambient endpoint variables are fine beside a typed endpoint; the
    // typed one wins inside the SDK.
    assert_eq!(
        resolve_endpoint(
            &typed,
            env(&[(OTEL_EXPORTER_OTLP_ENDPOINT, "http://other")])
        )
        .unwrap(),
        collector
    );
}

#[test]
fn environment_endpoint_enables_export_and_nothing_disables_it() {
    let untyped = options("");
    let root = (OTEL_EXPORTER_OTLP_ENDPOINT, "http://collector:4318");
    assert_eq!(resolve_endpoint(&untyped, env(&[])).unwrap(), None);
    assert_eq!(
        resolve_endpoint(&untyped, env(&[root])).unwrap(),
        Some(Endpoint::Environment)
    );
    // A blank typed endpoint is vacant, so the environment's is used.
    assert_eq!(
        resolve_endpoint(&options("  "), env(&[root])).unwrap(),
        Some(Endpoint::Environment)
    );
    // Ambient credentials are the platform's business when the platform
    // also supplies the endpoint.
    assert_eq!(
        resolve_endpoint(&untyped, env(&[root, (OTEL_EXPORTER_OTLP_HEADERS, "a=b")])).unwrap(),
        Some(Endpoint::Environment)
    );
    // The SDK would read a blank traces variable and fail, so the root is
    // handed over; with no root a blank variable leaves export disabled.
    assert_eq!(
        resolve_endpoint(
            &untyped,
            env(&[(OTEL_EXPORTER_OTLP_TRACES_ENDPOINT, " "), root])
        )
        .unwrap(),
        Some(Endpoint::EnvironmentRoot(
            "http://collector:4318".to_owned()
        ))
    );
    assert_eq!(
        resolve_endpoint(&untyped, env(&[(OTEL_EXPORTER_OTLP_TRACES_ENDPOINT, " ")])).unwrap(),
        None
    );
    assert_eq!(
        root_traces_url("http://collector:4318/"),
        "http://collector:4318/v1/traces"
    );
    assert_eq!(
        root_traces_url("http://collector:4318/otlp"),
        "http://collector:4318/otlp/v1/traces"
    );
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
            // A blank traces variant is vacant, so the general one holds.
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
    let headers = parse_headers("authorization=Basic%20x, x-tenant = t1 ,").unwrap();
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
        ("a=b,,bad name=x", 3),
        ("a=line\nbreak", 1),
    ] {
        let err = parse_headers(raw).unwrap_err();
        std::assert_matches!(err, TracingError::InvalidHeader { position: p } if p == position,
            "{raw:?}: {err}"
        );
    }
}

#[test]
fn collector_root_gets_the_traces_path() {
    assert_eq!(
        traces_url("http://collector:4318"),
        "http://collector:4318/v1/traces"
    );
    assert_eq!(
        traces_url("https://collector:4318/"),
        "https://collector:4318/v1/traces"
    );
    assert_eq!(
        traces_url("https://gateway.test/otlp/v1/traces"),
        "https://gateway.test/otlp/v1/traces"
    );
}

#[test]
fn https_otlp_endpoint_is_admitted_by_the_exporter_client() {
    use futures_util::FutureExt;

    // Without `reqwest-rustls`, reqwest rejects `https://` at export
    // with "URL scheme is not allowed". Construction succeeding is not
    // that proof; the request must run. The batch processor hosts the
    // blocking client off the async workers; a `#[tokio::test]` runtime
    // cannot drop reqwest's inner runtime. Nothing listens on :1, so
    // the accepted failure is a transport error, not collector delivery.
    let exporter = span_exporter(
        &Endpoint::Typed("https://127.0.0.1:1/v1/traces"),
        HashMap::new(),
        &CollectorTrust::default(),
        export_timeout(&|_: &str| None),
        &|_: &str| None,
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
fn gzip_compression_is_available_to_the_exporter() {
    // `compression` passes `gzip` programmatically; without the
    // `gzip-http` feature the build fails and tracing degrades.
    use opentelemetry_otlp::WithHttpConfig as _;

    opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_compression(opentelemetry_otlp::Compression::Gzip)
        .build()
        .expect("gzip must be a supported compression");
}

#[test]
fn compression_variables_select_gzip_or_none_and_refuse_the_rest() {
    let traces = OTEL_EXPORTER_OTLP_TRACES_COMPRESSION;
    let general = OTEL_EXPORTER_OTLP_COMPRESSION;

    assert_eq!(compression(&env(&[])).unwrap(), None);
    assert_eq!(
        compression(&env(&[(general, "GZIP")])).unwrap(),
        Some(opentelemetry_otlp::Compression::Gzip)
    );
    assert_eq!(
        compression(&env(&[(traces, "none"), (general, "gzip")])).unwrap(),
        None
    );
    assert_eq!(
        compression(&env(&[(traces, ""), (general, "gzip")])).unwrap(),
        Some(opentelemetry_otlp::Compression::Gzip)
    );
    for (pairs, refused) in [
        (&[(general, "zstd")][..], general),
        (&[(traces, "gzipp"), (general, "gzip")][..], traces),
    ] {
        std::assert_matches!(
                compression(&env(pairs)),
                Err(ExporterError::Compression { variable }) if variable == refused
            ,
            "{pairs:?}"
        );
    }
}
