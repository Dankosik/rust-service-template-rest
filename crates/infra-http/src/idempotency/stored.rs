use std::collections::BTreeMap;

use axum::body::{Body, Bytes};
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::{BufMut as _, BytesMut};
use infra_idempotency_store::{
    CaptureError, MAX_STORED_BODY_BYTES, Record, SuccessRecord, TerminalRefusal,
};
use serde::Serialize;
use utoipa::openapi::{Content, RefOr, Response as ApiResponse, Schema};
use utoipa::{IntoResponses, PartialSchema};

use crate::problem::{Code, PROBLEM_MEDIA_TYPE, Problem, http_status};

pub(super) use infra_idempotency_store::is_replayable_header_name;

const JSON: &str = "application/json";
const TEXT: &str = "text/plain";

pub trait ReplaySuccess: IntoResponses {
    #[must_use]
    fn schemas() -> Vec<(String, RefOr<Schema>)> {
        Vec::new()
    }

    fn capture(self) -> Result<ReplayableResponse, ReplayError>;
}

#[doc(hidden)]
#[must_use]
pub fn json_responses(
    status: StatusCode,
    description: &str,
    schema: RefOr<Schema>,
) -> BTreeMap<String, RefOr<ApiResponse>> {
    typed_response(status, description, JSON, schema)
}

#[derive(Debug)]
pub struct CreatedText {
    pub body: String,
    pub headers: HeaderMap,
}

impl CreatedText {
    #[must_use]
    pub fn new(body: impl Into<String>) -> Self {
        Self::with_headers(body, HeaderMap::new())
    }

    #[must_use]
    pub fn with_headers(body: impl Into<String>, headers: HeaderMap) -> Self {
        Self {
            body: body.into(),
            headers,
        }
    }
}

impl From<String> for CreatedText {
    fn from(body: String) -> Self {
        Self::new(body)
    }
}

impl IntoResponses for CreatedText {
    fn responses() -> BTreeMap<String, RefOr<ApiResponse>> {
        typed_response(StatusCode::CREATED, "created", TEXT, String::schema())
    }
}

impl ReplaySuccess for CreatedText {
    fn capture(mut self) -> Result<ReplayableResponse, ReplayError> {
        set_uncoded_content_type(&mut self.headers, TEXT)?;
        ReplayableResponse::bytes(StatusCode::CREATED, &self.headers, self.body.into())
    }
}

#[derive(Debug, Default)]
pub struct NoContent {
    pub headers: HeaderMap,
}

impl NoContent {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_headers(headers: HeaderMap) -> Self {
        Self { headers }
    }
}

impl IntoResponses for NoContent {
    fn responses() -> BTreeMap<String, RefOr<ApiResponse>> {
        let response = ApiResponse::builder().description("no content").build();
        BTreeMap::from([(StatusCode::NO_CONTENT.as_str().to_owned(), response.into())])
    }
}

impl ReplaySuccess for NoContent {
    fn capture(self) -> Result<ReplayableResponse, ReplayError> {
        ReplayableResponse::empty(StatusCode::NO_CONTENT, &self.headers)
    }
}

fn typed_response(
    status: StatusCode,
    description: &str,
    media_type: &str,
    schema: RefOr<Schema>,
) -> BTreeMap<String, RefOr<ApiResponse>> {
    let response = ApiResponse::builder()
        .description(description)
        .content(media_type, Content::new(Some(schema)))
        .build();
    BTreeMap::from([(status.as_str().to_owned(), response.into())])
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ReplayError(ReplayErrorKind);

impl ReplayError {
    const fn capture(error: CaptureError) -> Self {
        Self(ReplayErrorKind::Capture(error))
    }
}

#[derive(Debug, thiserror::Error)]
enum ReplayErrorKind {
    #[error(transparent)]
    Capture(CaptureError),
    #[error("content_encoding")]
    ContentEncoding,
    #[error("serialization")]
    Serialization,
}

#[derive(Clone, Debug)]
pub struct TerminalProblem(Problem);

impl TerminalProblem {
    pub(super) fn capture(&self) -> Result<TerminalRefusal, ReplayError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(PROBLEM_MEDIA_TYPE));
        TerminalRefusal::capture(http_status(self.0.code()), &headers, json_body(&self.0)?)
            .map_err(ReplayError::capture)
    }

    #[must_use]
    pub const fn conflict() -> Self {
        Self(Problem::new(Code::Conflict))
    }

    #[must_use]
    pub const fn already_exists() -> Self {
        Self(Problem::new(Code::AlreadyExists))
    }

    #[must_use]
    pub const fn unprocessable_content() -> Self {
        Self(Problem::new(Code::UnprocessableContent))
    }

    #[must_use]
    pub fn detail(self, detail: impl Into<compact_str::CompactString>) -> Self {
        Self(self.0.detail(detail))
    }

    #[must_use]
    pub fn instance(self, instance: impl Into<compact_str::CompactString>) -> Self {
        Self(self.0.instance(instance))
    }

    #[must_use]
    pub fn invalid_param(
        self,
        name: impl Into<compact_str::CompactString>,
        reason: impl Into<compact_str::CompactString>,
    ) -> Self {
        Self(self.0.invalid_param(name, reason))
    }
}

#[derive(Debug)]
pub struct ReplayableResponse(SuccessRecord);

impl ReplayableResponse {
    pub fn bytes(
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, ReplayError> {
        SuccessRecord::capture(status, headers, body)
            .map(Self)
            .map_err(ReplayError::capture)
    }

    pub fn json<T: Serialize + ?Sized>(
        status: StatusCode,
        mut headers: HeaderMap,
        value: &T,
    ) -> Result<Self, ReplayError> {
        set_uncoded_content_type(&mut headers, JSON)?;
        Self::bytes(status, &headers, json_body(value)?)
    }

    pub fn empty(status: StatusCode, headers: &HeaderMap) -> Result<Self, ReplayError> {
        Self::bytes(status, headers, Bytes::new())
    }

    pub(super) fn into_success(self) -> SuccessRecord {
        self.0
    }
}

pub(super) fn response_from_record(record: Record) -> Response {
    let (status, headers, body) = record.into_parts();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn set_uncoded_content_type(
    headers: &mut HeaderMap,
    media_type: &'static str,
) -> Result<(), ReplayError> {
    let mut has_encoding = false;
    for value in headers.get_all(CONTENT_ENCODING) {
        has_encoding = true;
        if value
            .as_bytes()
            .split(|&byte| byte == b',')
            .any(|coding| !coding.trim_ascii().eq_ignore_ascii_case(b"identity"))
        {
            return Err(ReplayError(ReplayErrorKind::ContentEncoding));
        }
    }
    if has_encoding {
        headers.remove(CONTENT_ENCODING);
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(media_type));
    Ok(())
}

fn json_body<T: Serialize + ?Sized>(value: &T) -> Result<Bytes, ReplayError> {
    let mut body = BytesMut::new();
    let mut writer = (&mut body).limit(MAX_STORED_BODY_BYTES).writer();
    serde_json::to_writer(&mut writer, value).map_err(|error| {
        if error.io_error_kind() == Some(std::io::ErrorKind::WriteZero) {
            ReplayError::capture(CaptureError::BodyLimit)
        } else {
            ReplayError(ReplayErrorKind::Serialization)
        }
    })?;
    Ok(body.freeze())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderName;
    use http_body_util::BodyExt as _;
    use utoipa::ToSchema;

    use super::*;

    #[tokio::test]
    async fn created_json_registers_the_payloads_of_boxed_recursive_and_list_responses() {
        #[derive(Serialize, ToSchema)]
        struct Address {
            street: String,
        }
        #[derive(Serialize, ToSchema)]
        struct Customer {
            address: Address,
            #[schema(no_recursion)]
            children: Vec<Customer>,
        }

        crate::created_json_response!(CreatedAddress(Box<Address>));
        crate::created_json_response!(CreatedCustomer(Box<Customer>));
        crate::created_json_response!(CreatedCustomers(Vec<Customer>));
        crate::json_response!(
            UpdatedAddress(Address),
            status = StatusCode::OK,
            description = "updated",
        );

        fn document<R: ReplaySuccess>() -> serde_json::Value {
            serde_json::json!({
                "responses": R::responses(),
                "components": {"schemas": R::schemas().into_iter().collect::<BTreeMap<_, _>>()},
            })
        }

        let updated = document::<UpdatedAddress>();
        assert_eq!(updated["responses"]["200"]["description"], "updated");
        assert_eq!(
            updated
                .pointer("/responses/200/content/application~1json/schema/$ref")
                .unwrap(),
            "#/components/schemas/Address"
        );
        let captured = UpdatedAddress::with_headers(
            Address {
                street: "Main".to_owned(),
            },
            header_map(&[("etag", "\"addr-1\"")]),
        )
        .capture()
        .unwrap();
        let response = response_from_record(Record::from(captured.into_success()));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()["etag"], "\"addr-1\"");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(br#"{"street":"Main"}"#)
        );

        let no_content = document::<NoContent>();
        assert_eq!(no_content["responses"]["204"]["description"], "no content");
        assert!(no_content["responses"]["204"].get("content").is_none());
        let captured = NoContent::with_headers(header_map(&[("etag", "\"deleted-1\"")]))
            .capture()
            .unwrap();
        let response = response_from_record(Record::from(captured.into_success()));
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["etag"], "\"deleted-1\"");
        assert!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );

        let root = "/responses/201/content/application~1json/schema";
        assert_eq!(
            document::<CreatedAddress>().pointer(root).unwrap()["$ref"],
            "#/components/schemas/Address"
        );
        for (doc, path) in [
            (document::<CreatedCustomer>(), root.to_owned()),
            (document::<CreatedCustomers>(), format!("{root}/items")),
        ] {
            assert_eq!(
                doc.pointer(&path).unwrap()["$ref"],
                "#/components/schemas/Customer"
            );
            let props = &doc["components"]["schemas"]["Customer"]["properties"];
            let child = props["children"]["items"]["$ref"].as_str().unwrap();
            assert_eq!(
                doc.pointer(child.strip_prefix('#').unwrap()).unwrap(),
                &doc["components"]["schemas"]["Customer"]
            );
            let address = props["address"]["$ref"].as_str().unwrap();
            assert_eq!(
                doc.pointer(address.strip_prefix('#').unwrap()).unwrap()["properties"]["street"]["type"],
                "string"
            );
        }
    }

    #[tokio::test]
    async fn response_constructors_default_to_empty_headers_and_support_from() {
        #[derive(Serialize, ToSchema)]
        struct Item {
            id: u32,
        }

        crate::created_json_response!(CreatedItem(Item));

        for json in [
            CreatedItem::new(Item { id: 7 }),
            CreatedItem::from(Item { id: 7 }),
        ] {
            let captured = json.capture().unwrap();
            let response = response_from_record(Record::from(captured.into_success()));
            assert_eq!(response.status(), StatusCode::CREATED);
            assert_eq!(response.headers().len(), 1);
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                Bytes::from_static(br#"{"id":7}"#)
            );
        }

        for text in [
            CreatedText::new("ok"),
            CreatedText::from("ok".to_owned()),
            CreatedText::with_headers("ok", HeaderMap::new()),
        ] {
            let captured = text.capture().unwrap();
            let response = response_from_record(Record::from(captured.into_success()));
            assert_eq!(response.status(), StatusCode::CREATED);
            assert_eq!(response.headers()[CONTENT_TYPE], "text/plain");
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                Bytes::from_static(b"ok")
            );
        }

        let empty = NoContent::new().capture().unwrap();
        let response = response_from_record(Record::from(empty.into_success()));
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.headers().is_empty());
    }

    fn header_map(headers: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn admission_reports_invalid_status_and_oversized_headers_as_rollback() {
        std::assert_matches!(
            ReplayableResponse::empty(StatusCode::CONFLICT, &HeaderMap::new()),
            Err(ReplayError(ReplayErrorKind::Capture(
                CaptureError::InvalidStatus
            )))
        );
        let large = "x".repeat(8192);
        std::assert_matches!(
            ReplayableResponse::empty(
                StatusCode::OK,
                &header_map(&[("content-type", large.as_str())])
            ),
            Err(ReplayError(ReplayErrorKind::Capture(
                CaptureError::HeaderLimit
            )))
        );
    }

    #[test]
    fn admission_rejects_nonempty_bodies_for_no_content_and_reset_content() {
        for status in [StatusCode::NO_CONTENT, StatusCode::RESET_CONTENT] {
            std::assert_matches!(
                ReplayableResponse::bytes(status, &HeaderMap::new(), Bytes::from_static(b"body")),
                Err(ReplayError(ReplayErrorKind::Capture(
                    CaptureError::BodyForbidden
                )))
            );
        }
    }

    #[tokio::test]
    async fn json_requires_uncoded_content() {
        for headers in [
            header_map(&[("content-encoding", "gzip")]),
            header_map(&[("content-encoding", "identity, gzip")]),
            header_map(&[("content-encoding", "identity"), ("content-encoding", "br")]),
        ] {
            std::assert_matches!(
                ReplayableResponse::json(StatusCode::OK, headers, &1),
                Err(ReplayError(ReplayErrorKind::ContentEncoding))
            );
        }

        let captured = ReplayableResponse::json(
            StatusCode::OK,
            header_map(&[("content-encoding", "IDENTITY")]),
            &1,
        )
        .unwrap();
        let response = response_from_record(Record::from(captured.into_success()));
        assert!(!response.headers().contains_key(CONTENT_ENCODING));
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(b"1")
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        #[test]
        fn the_response_carries_the_status_body_and_only_the_allow_listed_headers(
            status in 200_u16..300,
            body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
            headers in proptest::collection::vec(
                (
                    proptest::sample::select(vec![
                        "content-type", "content-encoding", "content-language",
                        "content-disposition", "location", "etag", "last-modified",
                        "set-cookie", "authorization", "x-trace", "idempotent-replayed",
                    ]),
                    "[ -~]{1,24}",
                ),
                0..12,
            ),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let pairs: Vec<(&str, &str)> =
                headers.iter().map(|(name, value)| (*name, value.as_str())).collect();
            let body = if matches!(status, 204 | 205) { Vec::new() } else { body };
            let captured = ReplayableResponse::bytes(
                StatusCode::from_u16(status).unwrap(), &header_map(&pairs), Bytes::from(body.clone())
            ).unwrap();
            let response = response_from_record(Record::from(captured.into_success()));

            proptest::prop_assert_eq!(response.status().as_u16(), status);
            let allowed = [
                "content-type", "content-encoding", "content-language", "content-disposition",
                "location", "etag", "last-modified",
            ];
            for name in response.headers().keys() {
                proptest::prop_assert!(allowed.contains(&name.as_str()), "{name}");
            }
            for name in allowed {
                let sent: Vec<_> = pairs
                    .iter()
                    .filter(|(sent, _)| *sent == name)
                    .map(|(_, value)| value.as_bytes())
                    .collect();
                let returned: Vec<_> =
                    response.headers().get_all(name).iter().map(HeaderValue::as_bytes).collect();
                proptest::prop_assert_eq!(returned, sent, "{}", name);
            }
            let returned_body = runtime
                .block_on(response.into_body().collect())
                .unwrap()
                .to_bytes();
            proptest::prop_assert_eq!(returned_body.as_ref(), body.as_slice());
        }
    }
}
