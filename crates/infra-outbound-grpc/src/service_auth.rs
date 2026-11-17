use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use http::header::AUTHORIZATION;
use infra_serviceauth::ReceiverTokens;
use tonic::body::Body;

use crate::Client;

#[derive(Clone, Debug)]
pub struct ServiceTokenClient {
    tokens: ReceiverTokens,
    resource: Client,
}

impl Client {
    #[must_use]
    pub fn with_service_tokens(&self, tokens: ReceiverTokens) -> ServiceTokenClient {
        ServiceTokenClient {
            tokens,
            resource: self.clone(),
        }
    }
}

impl tower::Service<http::Request<Body>> for ServiceTokenClient {
    type Response = http::Response<Body>;
    type Error = tonic::Status;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let mut prepared = self.resource.prepare_call(request);
        if prepared.headers_mut().contains_key(AUTHORIZATION) {
            return Box::pin(std::future::ready(Err(prepared.refuse(
                tonic::Status::internal("authorization conflicts with the service token"),
            ))));
        }
        let tokens = self.tokens.clone();
        Box::pin(prepared.send_inner(
            move |headers| {
                let authorization = tokens.authorization().map_err(|error| {
                    let mut status = tonic::Status::unavailable("service token unavailable");
                    status.set_source(Arc::new(error));
                    status
                })?;
                if let Some(value) = authorization {
                    headers.insert(AUTHORIZATION, value);
                }
                Ok(())
            },
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use infra_serviceauth::{
        JwtType, KeyId, ServiceName, SigningOptions, TokenSource, test_support::TestKey,
    };
    use tower::ServiceExt as _;

    use super::*;
    use crate::ClientSecurity;

    #[tokio::test]
    async fn service_token_client_redacts_debug_and_refuses_caller_authorization() {
        let key = TestKey::new("orders/2026-10");
        let source = TokenSource::try_new(SigningOptions {
            name: ServiceName::parse("orders").unwrap(),
            jwt_type: JwtType::parse("example-svc+jwt").unwrap(),
            key_id: KeyId::parse(key.kid()).unwrap(),
            private_key: key.pem().into(),
        })
        .unwrap();
        let client = Client::try_new(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            Duration::from_secs(1),
        )
        .unwrap();
        let bound = client.with_service_tokens(source.receiver("catalog").unwrap());

        let formatted = format!("{bound:?}");
        assert!(formatted.contains("catalog"), "{formatted}");
        assert!(
            !formatted.contains("PRIVATE KEY") && !formatted.contains("Bearer"),
            "{formatted}"
        );

        let request = http::Request::builder()
            .method("POST")
            .uri("/example.v1.EchoService/Unary")
            .header(AUTHORIZATION, "Bearer caller-token")
            .body(Body::empty())
            .unwrap();
        let status = bound.oneshot(request).await.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(
            status.message(),
            "authorization conflicts with the service token"
        );
    }
}
