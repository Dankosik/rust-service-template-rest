use futures_util::{StreamExt as _, stream::BoxStream};
use grpc_contracts::example::v1::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_server::{EchoService, EchoServiceServer},
};
use infra_grpc::{Code, Failure, RequestExt as _, ServiceRegistration, Services};
use tonic::{Request, Response, Status};

const MAX_MESSAGE_BYTES: usize = 1024;
const MAX_AGGREGATE_BYTES: usize = 1024;

type ReplyStream<T> = BoxStream<'static, Result<T, Status>>;

#[derive(Clone, Debug, Default)]
struct Echo;

pub(super) fn register(
    services: &mut Services,
    mut apply_caller_rule: impl FnMut(
        &mut ServiceRegistration<'_>,
        &str,
    ) -> Result<(), infra_grpc::Error>,
) -> Result<(), infra_grpc::Error> {
    services.describe(grpc_contracts::FILE_DESCRIPTOR_SET)?;
    let mut echo = services.add(EchoServiceServer::new(Echo))?;
    for method in ["Unary", "ClientStream", "ServerStream", "BidiStream"] {
        // template:begin grpc-authn:grpc-example-scope-requirements
        echo.require_scopes(method, &[])?;
        // template:end grpc-authn:grpc-example-scope-requirements
        apply_caller_rule(&mut echo, method)?;
    }
    Ok(())
}

#[tonic::async_trait]
impl EchoService for Echo {
    async fn unary(
        &self,
        request: Request<UnaryRequest>,
    ) -> Result<Response<UnaryResponse>, Status> {
        let message = accepted(request.into_inner().message)?;
        Ok(Response::new(UnaryResponse { message }))
    }

    async fn client_stream(
        &self,
        request: Request<tonic::Streaming<ClientStreamRequest>>,
    ) -> Result<Response<ClientStreamResponse>, Status> {
        let mut input = request.into_inner();
        let mut message = String::new();
        while let Some(next) = input.message().await? {
            let next = accepted(next.message)?;
            if message.len().saturating_add(next.len()) > MAX_AGGREGATE_BYTES {
                return Err(Failure::new(Code::RequestEntityTooLarge).into());
            }
            message.push_str(&next);
        }
        Ok(Response::new(ClientStreamResponse { message }))
    }

    type ServerStreamStream = ReplyStream<ServerStreamResponse>;

    async fn server_stream(
        &self,
        request: Request<ServerStreamRequest>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        let message = accepted(request.into_inner().message)?;
        Ok(Response::new(Box::pin(futures_util::stream::iter([Ok(
            ServerStreamResponse { message },
        )]))))
    }

    type BidiStreamStream = ReplyStream<BidiStreamResponse>;

    async fn bidi_stream(
        &self,
        request: Request<tonic::Streaming<BidiStreamRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        let context = request.response_context()?.clone();
        let responses = request
            .into_inner()
            .map(|item| {
                item.and_then(|request| {
                    let message = accepted(request.message)?;
                    Ok(BidiStreamResponse { message })
                })
            })
            .take_until(async move {
                context.operation().wait_stopped().await;
            });
        Ok(Response::new(Box::pin(responses)))
    }
}

fn accepted(message: String) -> Result<String, Status> {
    if (1..=MAX_MESSAGE_BYTES).contains(&message.len()) {
        Ok(message)
    } else {
        Err(Failure::new(Code::BadRequest)
            .field_violation("message", format!("must be 1 to {MAX_MESSAGE_BYTES} bytes"))
            .into())
    }
}
