//! Minimal all-cardinality transport example using the service bootstrap.
//!
//! This executable deliberately registers no default business service. It is
//! the one small feature adapter that proves a derived service uses the same
//! process, listener, admission, and shutdown path as the service binary.

use std::pin::Pin;
use std::process::ExitCode;

use futures_util::{Stream, StreamExt as _};
use grpc_contracts::generated::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_server::EchoService,
};
use infra_grpc::{Services, classified_status};
use service_failure::{ClassifiedFailure, Code};
use tonic::{Request, Response, Status};

const MAX_AGGREGATE_BYTES: usize = 1024;

#[derive(Clone, Debug, Default)]
struct Echo;

#[tonic::async_trait]
impl EchoService for Echo {
    async fn unary(
        &self,
        request: Request<UnaryRequest>,
    ) -> Result<Response<UnaryResponse>, Status> {
        Ok(Response::new(UnaryResponse {
            message: request.into_inner().message,
        }))
    }

    async fn client_stream(
        &self,
        request: Request<tonic::Streaming<ClientStreamRequest>>,
    ) -> Result<Response<ClientStreamResponse>, Status> {
        let mut input = request.into_inner();
        let mut message = String::new();
        while let Some(next) = input.message().await? {
            if message.len().saturating_add(next.message.len()) > MAX_AGGREGATE_BYTES {
                return Err(classified_status(ClassifiedFailure::new(
                    Code::RequestEntityTooLarge,
                )));
            }
            message.push_str(&next.message);
        }
        Ok(Response::new(ClientStreamResponse { message }))
    }

    type ServerStreamStream =
        Pin<Box<dyn Stream<Item = Result<ServerStreamResponse, Status>> + Send>>;

    async fn server_stream(
        &self,
        request: Request<ServerStreamRequest>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        let message = request.into_inner().message;
        Ok(Response::new(Box::pin(tokio_stream::iter([Ok(
            ServerStreamResponse { message },
        )]))))
    }

    type BidiStreamStream = Pin<Box<dyn Stream<Item = Result<BidiStreamResponse, Status>> + Send>>;

    async fn bidi_stream(
        &self,
        request: Request<tonic::Streaming<BidiStreamRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        let responses = request.into_inner().map(|item| {
            item.map(|request| BidiStreamResponse {
                message: request.message,
            })
        });
        Ok(Response::new(Box::pin(responses)))
    }
}

fn register(services: &mut Services) -> Result<(), infra_grpc::Error> {
    grpc_contracts::generated::register_echo_service(services, Echo)
}

fn main() -> ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
