use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::future::{BoxFuture, Either};
use http::header::AUTHORIZATION;
use infra_serviceauth::ReceiverTokens;
use operation_context::OperationContext;
use tonic::body::Body;

use crate::transport::PreparedCall;
use crate::{Client, ClientError, WithContext};

#[derive(Clone, Debug)]
pub struct ServiceTokenClient {
    tokens: ReceiverTokens,
    client: Client,
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
            client: self.clone(),
        })
    }
}

impl ServiceTokenClient {
    #[must_use]
    pub fn with_context(&self, context: &OperationContext) -> WithContext<Self> {
        WithContext::new(self.clone(), context)
    }

    fn prepare(&self, request: http::Request<Body>) -> Result<PreparedCall, tonic::Status> {
        let prepared = PreparedCall::prepare(&self.client, request)?;
        let (prepared, ()) = prepared.try_step(|_, headers| {
            if headers.contains_key(AUTHORIZATION) {
                return Err(tonic::Status::internal(
                    "authorization conflicts with the service token",
                ));
            }
            Ok(())
        })?;
        Ok(prepared)
    }
}

impl tower::Service<http::Request<Body>> for ServiceTokenClient {
    type Response = http::Response<Body>;
    type Error = tonic::Status;
    type Future = Either<
        std::future::Ready<Result<Self::Response, Self::Error>>,
        BoxFuture<'static, Result<Self::Response, Self::Error>>,
    >;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let prepared = match self.prepare(request) {
            Ok(prepared) => prepared,
            Err(status) => return Either::Left(std::future::ready(Err(status))),
        };
        let tokens = self.tokens.clone();
        Either::Right(Box::pin(prepared.send_with_dispatch(move |headers| {
            let authorization = tokens.authorization().map_err(|error| {
                let mut status = tonic::Status::unavailable("service token unavailable");
                status.set_source(Arc::new(error));
                status
            })?;
            if let Some(value) = authorization {
                headers.insert(AUTHORIZATION, value);
            }
            Ok(())
        })))
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::time::Duration;

    use infra_serviceauth::{ServiceName, TokenSource, test_support::TestKey};
    use tower::ServiceExt as _;

    use super::*;
    use crate::ClientSecurity;

    #[tokio::test]
    async fn service_token_client_redacts_debug_and_checks_stops_before_authorization() {
        let key = TestKey::new("orders/2026-10");
        let source =
            TokenSource::try_new(key.signing_options("orders", "example-svc+jwt")).unwrap();
        let catalog: ServiceName = "catalog".parse().unwrap();
        let origin = http::Uri::from_static("https://peer.example:443");
        let tokens = source.receiver(catalog.clone(), origin.clone()).unwrap();
        let plain = Client::builder(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            Duration::from_secs(1),
        )
        .build()
        .unwrap();
        assert_matches!(
            plain.with_service_tokens(tokens.clone()).unwrap_err(),
            ClientError::ServiceTokensRequireTls
        );
        let disabled = TokenSource::disabled()
            .receiver(catalog.clone(), origin)
            .unwrap();
        assert!(plain.with_service_tokens(disabled).is_ok());
        let ca = rcgen::generate_simple_self_signed(["peer.example".to_owned()]).unwrap();
        let ca_pem = ca.cert.pem();
        let client = Client::builder(
            "https://PEER.example",
            ClientSecurity::Tls(crate::ClientTlsMaterial {
                ca_certificate_pem: Some(&ca_pem),
                identity: None,
            }),
            Duration::from_secs(1),
        )
        .build()
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
            let elsewhere = source
                .receiver(catalog.clone(), wrong.parse().unwrap())
                .unwrap();
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
