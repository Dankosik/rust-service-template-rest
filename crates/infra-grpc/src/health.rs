use std::collections::BTreeSet;

use ::health::ReadinessReader;
use futures_util::stream::BoxStream;
use tonic::{Request, Response, Status};
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_server::Health;
use tonic_health::pb::{HealthCheckRequest, HealthCheckResponse};

pub(crate) type HealthServer = tonic_health::pb::health_server::HealthServer<Adapter>;

#[derive(Debug)]
pub(crate) struct Adapter {
    readiness: ReadinessReader,
    service_names: BTreeSet<&'static str>,
}

impl Adapter {
    pub(crate) fn new(
        readiness: ReadinessReader,
        mut service_names: BTreeSet<&'static str>,
    ) -> Self {
        service_names.insert(tonic_health::pb::health_server::SERVICE_NAME);
        Self {
            readiness,
            service_names,
        }
    }

    fn is_known(&self, name: &str) -> bool {
        name.is_empty() || self.service_names.contains(name)
    }
}

#[tonic::async_trait]
impl Health for Adapter {
    async fn check(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let service = request.get_ref().service.as_str();
        if !self.is_known(service) {
            return Err(Status::not_found("service not registered"));
        }
        let status = serving(self.readiness.is_ready());
        Ok(Response::new(response(status)))
    }

    type WatchStream = BoxStream<'static, Result<HealthCheckResponse, Status>>;

    async fn watch(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let known = self.is_known(&request.get_ref().service);
        let stream: Self::WatchStream = Box::pin(watch(self.readiness.clone(), known));
        Ok(Response::new(stream))
    }
}

fn watch(
    mut reader: ReadinessReader,
    known: bool,
) -> impl futures_util::Stream<Item = Result<HealthCheckResponse, Status>> + Send {
    async_stream::stream! {
        if !known {
            yield Ok(response(ServingStatus::ServiceUnknown));
            reader.wait_for_drain_or_owner_drop().await;
            return;
        }
        let mut ready = reader.is_ready();
        loop {
            yield Ok(response(serving(ready)));
            match reader.wait_until_ready_is_not(ready).await {
                Some(next) => ready = next,
                None => break,
            }
        }
    }
}

fn serving(ready: bool) -> ServingStatus {
    if ready {
        ServingStatus::Serving
    } else {
        ServingStatus::NotServing
    }
}

fn response(status: ServingStatus) -> HealthCheckResponse {
    HealthCheckResponse {
        status: status as i32,
    }
}
