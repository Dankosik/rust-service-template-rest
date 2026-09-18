"""One-off authoring script; removed with the temporary workflow before review."""
from pathlib import Path
import re


def replace_once(path: str, before: str, after: str) -> None:
    file = Path(path)
    text = file.read_text()
    if text.count(before) != 1:
        raise SystemExit(f'{path}: expected exactly one replacement anchor')
    file.write_text(text.replace(before, after, 1))


def replace_section(path: str, start: str, end: str, replacement: str) -> None:
    file = Path(path)
    text = file.read_text()
    if text.count(start) != 1 or text.count(end) != 1:
        raise SystemExit(f'{path}: section anchors are not unique')
    first = text.index(start)
    last = text.index(end, first)
    file.write_text(text[:first] + replacement + text[last:])


problem = 'crates/infra-http/src/problem.rs'
replace_once(problem,
             '#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]',
             '#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, strum::VariantArray)]')
file = Path(problem)
text, count = re.subn(
    r"    pub const ALL: &'static \[Code\] = &\[\n.*?    \];",
    "    pub const ALL: &'static [Code] = <Self as strum::VariantArray>::VARIANTS;",
    file.read_text(), flags=re.S)
if count != 1:
    raise SystemExit('expected exactly one Code::ALL array')
file.write_text(text)

harden = 'crates/infra-http/src/harden.rs'
replace_once(harden,
             '    use http_body_util::BodyExt;\n    use tower::ServiceExt;',
             '    use axum_test::TestServer;\n    use http_body_util::BodyExt;\n    use serde_json::Value;\n    use tower::ServiceExt;')
replace_section(harden,
    '    #[tokio::test]\n    async fn success_carries_request_id_and_nosniff()',
    '    #[tokio::test]\n    async fn declared_and_streamed_oversize_bodies_are_413_problems()',
    '''    #[tokio::test]
    async fn success_carries_request_id_and_nosniff() {
        let server = TestServer::new(app(&options()));
        let response = server.get("/ok").await;
        response.assert_status_ok();
        response.assert_header(X_CONTENT_TYPE_OPTIONS, "nosniff");
        response.assert_text("ok");
        let id = response.header(REQUEST_ID_HEADER);
        let id = id.to_str().unwrap();
        assert_eq!(id.len(), 36, "generated UUIDv4: {id}");
    }

    #[tokio::test]
    async fn valid_inbound_request_id_is_echoed_and_invalid_replaced() {
        let server = TestServer::new(app(&options()));
        server
            .get("/ok")
            .add_header(REQUEST_ID_HEADER, "client-id_1")
            .await
            .assert_header(REQUEST_ID_HEADER, "client-id_1");

        let response = server
            .get("/ok")
            .add_header(REQUEST_ID_HEADER, "bad id with spaces")
            .await;
        let id = response.header(REQUEST_ID_HEADER);
        assert_ne!(id, "bad id with spaces");
        assert_eq!(id.to_str().unwrap().len(), 36);
    }

    #[tokio::test]
    async fn not_found_and_method_not_allowed_are_problems_with_allow() {
        let server = TestServer::new(app(&options()));
        let response = server.get("/missing").await;
        response.assert_status(StatusCode::NOT_FOUND);
        response.assert_header(CONTENT_TYPE, "application/problem+json");
        let json = response.json::<Value>();
        assert_eq!(json["code"], "not_found");
        let id = response.header(REQUEST_ID_HEADER);
        assert_eq!(json["request_id"].as_str(), Some(id.to_str().unwrap()));

        let response = server.delete("/ok").await;
        response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
        response.assert_header(CONTENT_TYPE, "application/problem+json");
        let allow = response.header(ALLOW);
        assert!(allow.to_str().unwrap().contains("GET"), "{allow:?}");
        assert_eq!(response.json::<Value>()["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn timeout_is_a_504_problem() {
        let server = TestServer::new(app(&options()));
        let response = server.get("/slow").await;
        response.assert_status(StatusCode::GATEWAY_TIMEOUT);
        response.assert_header(CONTENT_TYPE, "application/problem+json");
        let json = response.json::<Value>();
        assert_eq!(json["code"], "gateway_timeout");
        let id = response.header(REQUEST_ID_HEADER);
        assert_eq!(json["request_id"].as_str(), Some(id.to_str().unwrap()));
    }

    #[tokio::test]
    async fn panic_is_a_sanitized_500_problem() {
        let server = TestServer::new(app(&options()));
        let response = server.get("/panic").await;
        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        response.assert_header(CONTENT_TYPE, "application/problem+json");
        assert!(response.headers().contains_key(&REQUEST_ID_HEADER));
        let json = response.json::<Value>();
        assert_eq!(json["code"], "internal_error");
        assert_eq!(json["detail"], SANITIZED_DETAIL);
        assert!(!json.to_string().contains("boom"));
    }

''')
replace_once(harden, '''        let response = app(&options)
            .oneshot(request(Method::GET, "/ok"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);''', '''        let server = TestServer::new(app(&options));
        server.get("/ok").await.assert_status_ok();''')
replace_once(harden, '''        let mut req = request(Method::OPTIONS, "/ok");
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://evil.example"));
        req.headers_mut().insert(
            "access-control-request-method",
            HeaderValue::from_static("GET"),
        );
        let response = app(&options()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);''', '''        let server = TestServer::new(app(&options()));
        let response = server
            .method(Method::OPTIONS, "/ok")
            .add_header("origin", "https://evil.example")
            .add_header("access-control-request-method", "GET")
            .await;
        response.assert_status(StatusCode::METHOD_NOT_ALLOWED);''')

validate = 'crates/config/src/validate.rs'
replace_once(validate, '''    #[test]
    fn socket_addr_accepts_go_style_port_only() {
        assert_eq!(
            socket_addr("k", ":8080").unwrap(),
            "0.0.0.0:8080".parse().unwrap()
        );
        assert_eq!(
            socket_addr("k", "[::1]:9000").unwrap(),
            "[::1]:9000".parse().unwrap()
        );
    }
''', '''    #[rstest::rstest]
    #[case::port_only(":8080", "0.0.0.0:8080")]
    #[case::ipv6("[::1]:9000", "[::1]:9000")]
    #[case::ipv4("127.0.0.1:8080", "127.0.0.1:8080")]
    #[case::trimmed("  :8080  ", "0.0.0.0:8080")]
    fn socket_addr_accepts_supported_forms(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(
            socket_addr("k", input).unwrap(),
            expected.parse::<std::net::SocketAddr>().unwrap()
        );
    }

    #[rstest::rstest]
    #[case::below_minimum(0, false)]
    #[case::minimum(1, true)]
    #[case::maximum(10, true)]
    #[case::above_maximum(11, false)]
    fn int_range_is_inclusive(#[case] value: u64, #[case] valid: bool) {
        assert_eq!(int_range("limit", value, 1, 10).is_ok(), valid);
    }

    #[rstest::rstest]
    #[case::below_minimum(99, false)]
    #[case::minimum(100, true)]
    #[case::maximum(600_000, true)]
    #[case::above_maximum(600_001, false)]
    fn duration_range_is_inclusive(#[case] milliseconds: u64, #[case] valid: bool) {
        let result = duration_range(
            "http.request_timeout",
            Duration::from_millis(milliseconds),
            Duration::from_millis(100),
            Duration::from_secs(600),
        );
        assert_eq!(result.is_ok(), valid);
    }
''')

openapi = Path('crates/service/tests/openapi.rs')
text = openapi.read_text()
start = '#[test]\nfn bearer_security_alternatives_fail_closed() {'
if text.count(start) != 1 or not text.rstrip().endswith('}'):
    raise SystemExit('OpenAPI classifier test anchor is not unique')
text = text[:text.index(start)] + '''#[rstest::rstest]
#[case::inherited_bearer(&json!({}), false, true)]
#[case::explicit_bearer(&json!({"security": [{"bearerAuth": []}]}), false, true)]
#[case::explicit_public(&json!({"security": []}), true, false)]
#[case::anonymous_alternative(&json!({"security": [{"bearerAuth": []}, {}]}), false, false)]
#[case::unauthorized_scopes(&json!({"security": [{"bearerAuth": ["admin"]}]}), false, false)]
#[case::unknown_scheme(&json!({"security": [{"missingAuth": []}]}), false, false)]
#[case::unsupported_alternative(&json!({"security": [{"apiKeyAuth": []}]}), false, false)]
#[case::unsupported_and_requirement(
    &json!({"security": [{"bearerAuth": [], "apiKeyAuth": []}]}),
    false,
    false
)]
fn bearer_security_alternatives_fail_closed(
    #[case] operation: &Value,
    #[case] public: bool,
    #[case] protected: bool,
) {
    let document = json!({
        "components": {"securitySchemes": {
            "bearerAuth": {"type": "http", "scheme": "bearer"},
            "apiKeyAuth": {"type": "apiKey", "in": "header", "name": "X-API-Key"}
        }},
        "security": [{"bearerAuth": []}]
    });
    assert_eq!(is_public(&document, operation), public);
    assert_eq!(uses_bearer_only(&document, operation), protected);
}
'''
openapi.write_text(text)

replace_once('CONTRIBUTING.md', '## Security and ownership\n', '''## Library selection

[Backend library selection](docs/backend-library-selection.md) records the
adopted test helpers and the triggers for validators, serialization adapters,
builders, SQLx query tooling and optional integrations. Consult it before
adding a dependency or writing a generic mechanism. Deferred candidates are
not a mandatory install list; the owning feature must justify and validate
its choice.

## Security and ownership
''')
replace_once('docs/first-production-feature.md', '''table too (`greeting = { path = "crates/greeting" }` under the workspace
crates in `Cargo.toml`).
''', '''table too (`greeting = { path = "crates/greeting" }` under the workspace
crates in `Cargo.toml`).

Consult [Backend library selection](backend-library-selection.md) before
adding validators, serialization adapters, builders, persistence machinery or
outbound clients. It distinguishes adopted test helpers from choices that
need a real feature; do not install the deferred libraries speculatively.
''')
replace_once('docs/first-production-feature.md', '''`greets_with_json` and `reserved_name_is_a_not_found_problem`.
''', '''`greets_with_json` and `reserved_name_is_a_not_found_problem`.

For new ordinary router tests, the workspace now provides `axum-test` as a
dev-dependency: `TestServer::new(router::<()>().split_for_parts().0)` uses the
in-process transport. The hardened-chain tests demonstrate request, header,
status and JSON assertions. Keep the direct `oneshot` form above where raw
bodies, framing, concurrency or response extensions are the subject.
''')
