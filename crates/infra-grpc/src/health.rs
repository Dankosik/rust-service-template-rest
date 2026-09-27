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

fn watch(
    reader: ReadinessReader,
    known: bool,
) -> impl futures_util::Stream<Item = Result<HealthCheckResponse, Status>> + Send {
    let state = if known {
        Watch::Known {
            reader,
            previous: None,
        }
    } else {
        Watch::Unknown {
            reader,
            sent: false,
        }
    };
    futures_util::stream::unfold(state, step)
}

enum Watch {
    Known {
        reader: ReadinessReader,
        previous: Option<ServingStatus>,
    },
    Unknown {
        reader: ReadinessReader,
        sent: bool,
    },
    Finished,
}

async fn step(state: Watch) -> Option<(Result<HealthCheckResponse, Status>, Watch)> {
    match state {
        Watch::Unknown {
            reader,
            sent: false,
        } => Some((
            Ok(response(ServingStatus::ServiceUnknown)),
            Watch::Unknown { reader, sent: true },
        )),
        Watch::Unknown {
            mut reader,
            sent: true,
        } => loop {
            if matches!(reader.verdict(), Err(NotReady::Draining)) {
                return None;
            }
            if reader.changed_verdict().await.is_err() {
                return None;
            }
        },
        Watch::Known {
            reader,
            previous: None,
        } => {
            let status = serving(&reader);
            let next = if matches!(reader.verdict(), Err(NotReady::Draining)) {
                Watch::Finished
            } else {
                Watch::Known {
                    reader,
                    previous: Some(status),
                }
            };
            Some((Ok(response(status)), next))
        }
        Watch::Known {
            mut reader,
            previous: Some(previous),
        } => loop {
            if reader.changed_verdict().await.is_err() {
                return None;
            }
            if matches!(reader.verdict(), Err(NotReady::Draining)) {
                if previous == ServingStatus::NotServing {
                    return None;
                }
                return Some((Ok(response(ServingStatus::NotServing)), Watch::Finished));
            }
            let status = serving(&reader);
            if status != previous {
                return Some((
                    Ok(response(status)),
                    Watch::Known {
                        reader,
                        previous: Some(status),
                    },
                ));
            }
        },
        Watch::Finished => None,
    }
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
