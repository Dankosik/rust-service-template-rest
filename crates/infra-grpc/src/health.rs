use std::collections::BTreeSet;

use ::health::{NotReady, ReadinessReader};
use tonic::{Request, Response, Status};
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_server::Health;
use tonic_health::pb::{HealthCheckRequest, HealthCheckResponse};

/// Readiness-backed standard health service.
#[derive(Debug)]
pub(crate) struct Adapter {
    readiness: ReadinessReader,
    services: BTreeSet<&'static str>,
}

impl Adapter {
    pub(crate) fn new(readiness: ReadinessReader, names: &BTreeSet<&'static str>) -> Self {
        let mut services = names.clone();
        services.insert(tonic_health::pb::health_server::SERVICE_NAME);
        Self {
            readiness,
            services,
        }
    }

    fn known(&self, name: &str) -> bool {
        name.is_empty() || self.services.contains(name)
    }
}

#[tonic::async_trait]
impl Health for Adapter {
    async fn check(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let service = request.get_ref().service.as_str();
        if !self.known(service) {
            return Err(Status::not_found("service not registered"));
        }
        Ok(Response::new(response(serving(&self.readiness))))
    }

    type WatchStream = tonic::codegen::BoxStream<HealthCheckResponse>;

    async fn watch(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let known = self.known(&request.get_ref().service);
        let stream: Self::WatchStream = Box::pin(watch(self.readiness.clone(), known));
        Ok(Response::new(stream))
    }
}

/// Sends the current status, then each change. Ends after reporting drain, so
/// watchers never hold the listener drain. An unknown service is reported as
/// `SERVICE_UNKNOWN` and the stream stays open, as the health protocol requires.
fn watch(
    reader: ReadinessReader,
    known: bool,
) -> impl futures_util::Stream<Item = Result<HealthCheckResponse, Status>> + Send {
    futures_util::stream::unfold(Some((reader, None)), move |state| async move {
        let (mut reader, sent) = state?;
        loop {
            let status = if known {
                serving(&reader)
            } else {
                ServingStatus::ServiceUnknown
            };
            let draining = matches!(reader.verdict(), Err(NotReady::Draining));
            if sent != Some(status) {
                let next = (!draining).then_some((reader, Some(status)));
                return Some((Ok(response(status)), next));
            }
            if draining || reader.changed_verdict().await.is_err() {
                return None;
            }
        }
    })
}

fn serving(reader: &ReadinessReader) -> ServingStatus {
    if reader.verdict().is_ok() {
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
