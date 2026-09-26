use std::collections::BTreeSet;
use std::sync::Arc;

use ::health::ReadinessReader;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tonic::{Request, Response, Status};
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_server::{Health, HealthServer};
use tonic_health::pb::{HealthCheckRequest, HealthCheckResponse};

use crate::registration::Registry;
use crate::server::HealthState;

const HEALTH_SERVICE: &str = "grpc.health.v1.Health";

pub(crate) fn server(
    readiness: ReadinessReader,
    registry: &Registry,
    startup: Arc<HealthState>,
    cancel: CancellationToken,
    tracker: TaskTracker,
) -> HealthServer<Adapter> {
    let mut services = registry.services().collect::<BTreeSet<_>>();
    services.insert(HEALTH_SERVICE);
    HealthServer::new(Adapter {
        readiness,
        services: Arc::new(services),
        startup,
        cancel,
        tracker,
    })
    .max_decoding_message_size(4 * 1024 * 1024)
    .max_encoding_message_size(4 * 1024 * 1024)
}

#[derive(Clone)]
pub(crate) struct Adapter {
    readiness: ReadinessReader,
    services: Arc<BTreeSet<&'static str>>,
    startup: Arc<HealthState>,
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl Adapter {
    fn known(&self, name: &str) -> bool {
        name.is_empty() || self.services.contains(name)
    }

    fn status(&self) -> ServingStatus {
        if self.startup.is_open() && self.readiness.verdict().is_ok() {
            ServingStatus::Serving
        } else {
            ServingStatus::NotServing
        }
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
        Ok(Response::new(response(self.status())))
    }

    type WatchStream = tokio_stream::wrappers::ReceiverStream<Result<HealthCheckResponse, Status>>;

    async fn watch(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let service = request.get_ref().service.clone();
        let known = self.known(&service);
        let (sender, receiver) = mpsc::channel(4);
        let mut readiness = self.readiness.clone();
        let startup = Arc::clone(&self.startup);
        let mut startup_changes = startup.subscribe();
        let cancel = self.cancel.child_token();
        self.tracker.spawn(async move {
            let initial = if known {
                serving(&readiness, &startup)
            } else {
                ServingStatus::ServiceUnknown
            };
            tokio::select! {
                () = cancel.cancelled() => return,
                sent = sender.send(Ok(response(initial))) => {
                    if sent.is_err() { return; }
                }
            }
            if !known {
                tokio::select! {
                    () = cancel.cancelled() => {},
                    () = sender.closed() => {},
                }
                return;
            }

            let mut previous = initial;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = sender.closed() => return,
                    changed = readiness.changed_verdict() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    changed = startup_changes.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
                let next = serving(&readiness, &startup);
                if next != previous {
                    previous = next;
                    tokio::select! {
                        () = cancel.cancelled() => return,
                        sent = sender.send(Ok(response(next))) => {
                            if sent.is_err() { return; }
                        }
                    }
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            receiver,
        )))
    }
}

fn serving(readiness: &ReadinessReader, startup: &HealthState) -> ServingStatus {
    if startup.is_open() && readiness.verdict().is_ok() {
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
