use axum::body::Bytes;
use axum::extract::{FromRequestParts as _, OriginalUri};
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, Method};
use http_body_util::Collected;
use infra_idempotency_store::RequestFingerprint;
use serde::de::DeserializeOwned;

use super::identity::request_fingerprint;
use crate::Problem;
use crate::extract::{Path, decode_json, decode_query, require_json_content_type};

#[derive(derive_more::Debug)]
#[debug("CommandInput {{ .. }}")]
pub struct CommandInput {
    parts: Parts,
    body: Bytes,
    trailers: HeaderMap,
}

impl CommandInput {
    pub(super) fn capture(
        mut parts: Parts,
        fingerprint_headers: &[HeaderName],
        collected: Collected<Bytes>,
    ) -> (Self, RequestFingerprint) {
        if let Some(original) = parts.extensions.remove::<OriginalUri>() {
            parts.uri = original.0;
        }
        parts.headers = select_sensitive_headers(
            &parts.headers,
            std::iter::once(&CONTENT_TYPE).chain(fingerprint_headers),
        );
        let trailers = collected.trailers().map_or_else(HeaderMap::new, |fields| {
            let mut trailers = fields.clone();
            for value in trailers.values_mut() {
                value.set_sensitive(true);
            }
            trailers
        });
        let input = Self {
            parts,
            body: collected.to_bytes(),
            trailers,
        };
        let fingerprint = request_fingerprint(&input, fingerprint_headers);
        (input, fingerprint)
    }

    #[must_use]
    pub fn method(&self) -> &Method {
        &self.parts.method
    }

    #[must_use]
    pub fn raw_path(&self) -> &str {
        self.parts.uri.path()
    }

    #[must_use]
    pub fn raw_query(&self) -> Option<&str> {
        self.parts.uri.query()
    }

    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.parts.headers
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    #[must_use]
    pub fn trailers(&self) -> &HeaderMap {
        &self.trailers
    }

    pub async fn path<T: DeserializeOwned + Send>(&mut self) -> Result<T, Problem> {
        Path::from_request_parts(&mut self.parts, &())
            .await
            .map(|Path(value)| value)
    }

    pub fn query<T: DeserializeOwned>(&self) -> Result<T, Problem> {
        decode_query(self.raw_query().unwrap_or_default())
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Problem> {
        require_json_content_type(self.parts.headers.get(CONTENT_TYPE))?;
        decode_json(&self.body)
    }
}

fn select_sensitive_headers<'a>(
    source: &HeaderMap,
    names: impl IntoIterator<Item = &'a HeaderName>,
) -> HeaderMap {
    let mut selected = HeaderMap::new();
    for name in names {
        for value in source.get_all(name) {
            let mut value = value.clone();
            value.set_sensitive(true);
            selected.append(name.clone(), value);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{HeaderValue, StatusCode};
    use http_body_util::{BodyExt as _, Full};
    use serde::Deserialize;
    use tower::ServiceExt as _;

    use super::*;

    #[derive(Deserialize)]
    struct Options {
        mode: String,
    }

    #[tokio::test]
    async fn nested_command_sees_fingerprinted_wire_inputs_and_typed_path_captures() {
        async fn decode(request: Request) {
            let (parts, body) = request.into_parts();
            let (mut input, fingerprint) = CommandInput::capture(
                parts,
                &[HeaderName::from_static("x-choice")],
                body.collect().await.unwrap(),
            );
            assert_eq!(
                fingerprint.as_bytes(),
                &const_hex::decode_to_array::<_, 32>(
                    "1ddbafb4293070230eb56b5d60eb92c9df7019b7a0153a6eca7bcd7602e02c1f"
                )
                .unwrap()
            );
            assert_eq!(input.method(), Method::POST);
            assert_eq!(input.raw_path(), "/accounts/a%20b/widgets/12");
            assert_eq!(input.raw_query(), Some("mode=full"));
            assert_eq!(input.query::<Options>().unwrap().mode, "full");
            assert_eq!(
                input.path::<(String, u32)>().await.unwrap(),
                ("a b".to_owned(), 12)
            );
            assert_eq!(input.headers().len(), 3);
            assert_eq!(input.headers()[CONTENT_TYPE], "application/json");
            assert_eq!(
                input
                    .headers()
                    .get_all("x-choice")
                    .iter()
                    .collect::<Vec<_>>(),
                ["b", "a"]
            );
            assert!(
                input
                    .headers()
                    .get_all("x-choice")
                    .iter()
                    .all(HeaderValue::is_sensitive)
            );
            assert_eq!(input.body(), br#""data""#);
            assert_eq!(input.json::<String>().unwrap(), "data");
            assert_eq!(input.trailers()["x-test-trailer"], "kept");
        }

        let mut trailers = HeaderMap::new();
        trailers.insert("x-test-trailer", HeaderValue::from_static("kept"));
        let request =
            Request::post("https://unrelated.example/accounts/a%20b/widgets/12?mode=full")
                .header(CONTENT_TYPE, "application/json")
                .header("x-choice", "b")
                .header("x-choice", "a")
                .header("cookie", "hidden=1")
                .header("x-private", "hidden")
                .header("idempotency-key", "hidden")
                .extension("unrelated middleware extension")
                .body(Body::new(
                    Full::new(Bytes::from_static(br#""data""#))
                        .with_trailers(std::future::ready(Some(Ok::<_, Infallible>(trailers)))),
                ))
                .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "this fixture exercises Axum's path parameters across a real nest"
        )]
        let app = axum::Router::new().nest(
            "/accounts/{account}",
            axum::Router::new().route("/widgets/{id}", axum::routing::post(decode)),
        );
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }
}
