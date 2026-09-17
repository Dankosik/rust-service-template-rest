//! One structured line per request.
//!
//! Runs inside the OpenTelemetry server span, so the line carries the trace
//! and span ids through the subscriber, and outside the shedder and timeout,
//! so a rejected or timed-out request is still recorded with its real status.
//! Matched health probe routes are skipped by route template, not raw path,
//! so an unmatched request that merely looks like a probe is still logged.

use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;

use crate::problem::Code;
use crate::request_id::request_id;
use crate::router::HEALTH_PROBE_ROUTES;

/// Label for requests the router did not match.
pub(crate) const UNMATCHED_ROUTE: &str = "<unmatched>";

#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessLogOptions {
    pub(crate) log_health_probes: bool,
}

pub(crate) async fn record(
    State(options): State<AccessLogOptions>,
    matched: Option<MatchedPath>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let route = matched
        .as_ref()
        .map_or(UNMATCHED_ROUTE, MatchedPath::as_str)
        .to_owned();
    let id = request_id(request.extensions());
    if let Some(id) = &id {
        // The server span pre-declares this field; recording it here puts
        // the id on every log line and exported span of the request.
        tracing::Span::current().record("request_id", id.as_str());
    }

    let response = next.run(request).await;

    if skip_probe(options, &method, &route) {
        return response;
    }
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let status = response.status().as_u16();
    // problem_code separates the failures that share a status: a 503 is load
    // shedding, a saturated pool, or a draining instance, and during an
    // incident that distinction is the whole question. Bounded by the catalog.
    let problem_code = response
        .extensions()
        .get::<Code>()
        .map(|code| code.as_str());
    tracing::info!(
        method = %method,
        route = %route,
        status,
        duration_ms,
        problem_code,
        request_id = id.as_deref(),
        "http_request"
    );
    response
}

fn skip_probe(options: AccessLogOptions, method: &Method, route: &str) -> bool {
    !options.log_health_probes && method == Method::GET && HEALTH_PROBE_ROUTES.contains(&route)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_are_skipped_by_route_template_only() {
        let quiet = AccessLogOptions {
            log_health_probes: false,
        };
        assert!(skip_probe(quiet, &Method::GET, "/health/live"));
        assert!(skip_probe(quiet, &Method::GET, "/health/ready"));
        assert!(!skip_probe(quiet, &Method::POST, "/health/live"));
        assert!(!skip_probe(quiet, &Method::GET, UNMATCHED_ROUTE));
        let verbose = AccessLogOptions {
            log_health_probes: true,
        };
        assert!(!skip_probe(verbose, &Method::GET, "/health/live"));
    }
}
