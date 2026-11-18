use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use http::header::AUTHORIZATION;
use infra_serviceauth::ReceiverTokens;
use tonic::body::Body;

use crate::{Client, ClientError, PreparedCall};

#[derive(Clone, Debug)]
pub struct ServiceTokenClient {
    tokens: ReceiverTokens,
    resource: Client,
}

impl Client {
    pub fn with_service_tokens(
        &self,
        tokens: ReceiverTokens,
    ) -> Result<ServiceTokenClient, ClientError> {
        if let Some(origin) = tokens.signing_origin() {
            if !self.uses_tls() {
                return Err(ClientError::ServiceTokensRequireTls);
            }
            if !self.matches_origin(origin) {
                return Err(ClientError::ServiceTokenOriginMismatch);
            }
        }
        Ok(ServiceTokenClient {
            tokens,
            resource: self.clone(),
        })
    }
}

impl ServiceTokenClient {
    fn prepare(&self, request: http::Request<Body>) -> Result<PreparedCall, tonic::Status> {
        let mut prepared = self.resource.prepare_call(request)?;
        if prepared.headers_mut().contains_key(AUTHORIZATION) {
            return Err(prepared.refuse(tonic::Status::internal(
                "authorization conflicts with the service token",
            )));
        }
        Ok(prepared)
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
        let prepared = match self.prepare(request) {
            Ok(prepared) => prepared,
            Err(status) => return Box::pin(std::future::ready(Err(status))),
        };
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
    use std::assert_matches;
    use std::time::Duration;

    use infra_serviceauth::{
        JwtType, KeyId, ServiceName, SigningOptions, TokenSource, test_support::TestKey,
    };
    use operation_context::OperationContext;
    use tower::ServiceExt as _;

    use super::*;
    use crate::ClientSecurity;

    #[tokio::test]
    async fn service_token_client_redacts_debug_and_checks_stops_before_authorization() {
        let key = TestKey::new("orders/2026-10");
        let source = TokenSource::try_new(SigningOptions {
            name: ServiceName::parse("orders").unwrap(),
            jwt_type: JwtType::parse("example-svc+jwt").unwrap(),
            key_id: KeyId::parse(key.kid()).unwrap(),
            private_key: key.pem().into(),
        })
        .unwrap();
        let origin = http::Uri::from_static("https://peer.example:443");
        let tokens = source.receiver("catalog", origin.clone()).unwrap();
        let plain = Client::try_new(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_matches!(
            plain.with_service_tokens(tokens.clone()).unwrap_err(),
            ClientError::ServiceTokensRequireTls
        );
        let disabled = TokenSource::disabled().receiver("catalog", origin).unwrap();
        assert!(plain.with_service_tokens(disabled).is_ok());
        let ca = rcgen::generate_simple_self_signed(["peer.example".to_owned()]).unwrap();
        let client = Client::try_new(
            "https://PEER.example",
            ClientSecurity::Tls(crate::ClientTlsMaterial {
                ca_certificate_pem: Some(ca.cert.pem()),
                identity: None,
            }),
            Duration::from_secs(1),
        )
        .unwrap();
        for wrong in [
            "https://other.example",
            "https://peer.example:444",
            "http://peer.example:443",
            "https://peer.example/path",
            "https://peer.example/?query",
            "https://user@peer.example",
            "https://peer.example:65536",
        ] {
            let elsewhere = source.receiver("catalog", wrong.parse().unwrap()).unwrap();
            assert_matches!(
                client.with_service_tokens(elsewhere).unwrap_err(),
                ClientError::ServiceTokenOriginMismatch
            );
        }
        let bound = client.with_service_tokens(tokens).unwrap();

        let formatted = format!("{bound:?}");
        assert!(formatted.contains("catalog"), "{formatted}");
        assert!(
            !formatted.contains("PRIVATE KEY") && !formatted.contains("Bearer"),
            "{formatted}"
        );

        let cancelled = OperationContext::unbounded();
        cancelled.cancel();
        for (context, code, message) in [
            (
                OperationContext::unbounded(),
                tonic::Code::Internal,
                "authorization conflicts with the service token",
            ),
            (cancelled, tonic::Code::Cancelled, "request cancelled"),
            (
                OperationContext::with_timeout(Duration::ZERO),
                tonic::Code::DeadlineExceeded,
                "request deadline exceeded",
            ),
        ] {
            let request = http::Request::builder()
                .method("POST")
                .uri("/example.v1.EchoService/Unary")
                .header(AUTHORIZATION, "Bearer caller-token")
                .extension(context)
                .body(Body::empty())
                .unwrap();
            let status = bound.clone().oneshot(request).await.unwrap_err();
            assert_eq!(status.code(), code);
            assert_eq!(status.message(), message);
        }
    }
}
