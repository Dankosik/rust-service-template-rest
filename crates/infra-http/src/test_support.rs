use axum::response::Response;
use http_body_util::BodyExt as _;

pub(crate) async fn body_json(response: Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
