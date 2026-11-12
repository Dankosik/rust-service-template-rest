use std::collections::BTreeSet;

use ::health::{NotReady, ReadinessReader};
use tonic::{Request, Response, Status};
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_server::Health;
use tonic_health::pb::{HealthCheckRequest, HealthCheckResponse};

pub(crate) type HealthServer = tonic_health::pb::health_server::HealthServer<Adapter>;

/// Readiness-backed standard health service.
#[derive(Debug)]
pub(crate) struct Adapter {
    readiness: ReadinessReader,
    services: BTreeSet<&'static str>,
}

impl Adapter {
    pub(crate) fn new(readiness: ReadinessReader, mut services: BTreeSet<&'static str>) -> Self {
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
        Ok(Response::new(response(serving(&self.readiness.verdict()))))
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
    mut reader: ReadinessReader,
    known: bool,
) -> impl futures_util::Stream<Item = Result<HealthCheckResponse, Status>> + Send {
    async_stream::stream! {
        let mut sent = None;
        loop {
            // Drop the verdict before awaiting so it is not held across the wait.
            let (status, draining) = {
                let verdict = reader.verdict();
                let status = if known {
                    serving(&verdict)
                } else {
                    ServingStatus::ServiceUnknown
                };
                (status, matches!(verdict, Err(NotReady::Draining)))
            };
            if sent != Some(status) {
                sent = Some(status);
                yield Ok(response(status));
            }
            if draining || reader.wait_for_verdict_event().await.is_err() {
                break;
            }
        }
    }
}

fn serving(verdict: &Result<(), NotReady>) -> ServingStatus {
    if verdict.is_ok() {
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
