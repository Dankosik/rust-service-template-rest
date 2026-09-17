//! Composition root: runtime, dependencies, transports, and process lifecycle.
//!
//! Startup order: runtime, logging, listen address, HTTP bind, readiness on.
//! Shutdown order: readiness off, drain HTTP inside one budget, exit.
//! Handlers and feature code never own this sequence.

mod listen;
mod shutdown;

use std::time::Duration;

use infra_http::{Readiness, Server, ServerError};

pub(crate) use listen::ListenAddrError;

/// Upper bound for draining in-flight HTTP requests after the stop signal.
///
/// Deployment grace periods must exceed this value. It becomes a validated
/// configuration key with the configuration stage of the roadmap.
const HTTP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(25);

/// Startup or shutdown failures reported by the process.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    #[error("build tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error(transparent)]
    ListenAddr(#[from] ListenAddrError),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("http drain exceeded {budget:?}")]
    ShutdownTimeout { budget: Duration },
}

/// Build the runtime and run the service until a stop signal completes drain.
pub(crate) fn run() -> Result<(), BootstrapError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(BootstrapError::Runtime)?;
    runtime.block_on(async {
        init_logging();
        let addr = listen::listen_addr_from_env()?;
        serve_until(addr, shutdown::signal()).await
    })
}

/// Serve on `addr` until `stop` resolves, then drain within the budget.
///
/// Separated from [`run`] so tests can inject the stop signal and an
/// ephemeral port without touching process signals.
pub(crate) async fn serve_until(
    addr: std::net::SocketAddr,
    stop: impl Future<Output = ()>,
) -> Result<(), BootstrapError> {
    let readiness = Readiness::new();
    let server = Server::bind(addr, infra_http::router(readiness.clone())).await?;
    tracing::info!(addr = %server.local_addr(), "http listener bound");
    readiness.set_ready(true);

    stop.await;

    readiness.set_ready(false);
    tracing::info!(budget = ?HTTP_SHUTDOWN_TIMEOUT, "stop signal received, draining http");
    match tokio::time::timeout(HTTP_SHUTDOWN_TIMEOUT, server.shutdown()).await {
        Ok(Ok(())) => {
            tracing::info!("http drained");
            Ok(())
        }
        Ok(Err(err)) => Err(err.into()),
        Err(_elapsed) => Err(BootstrapError::ShutdownTimeout {
            budget: HTTP_SHUTDOWN_TIMEOUT,
        }),
    }
}

/// Human-readable logs filtered by `RUST_LOG`, defaulting to `info`.
///
/// Structured JSON output and level configuration arrive with the
/// observability stage; keep this the only subscriber installation.
fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // A second installation in one process is a programming error, not a
    // runtime condition; ignore it rather than fail the service.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    async fn get(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn serves_ready_then_drains_on_stop() {
        // Bind through the same path as production, but on an ephemeral port
        // that the test discovers by asking the OS to hand it back.
        let probe = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let lifecycle = tokio::spawn(serve_until(addr, async move {
            let _ = stop_rx.await;
        }));

        // Wait for the listener to come up, then observe readiness.
        let mut response = String::new();
        for _ in 0..50 {
            if TcpStream::connect(addr).await.is_ok() {
                response = get(addr, "/health/ready").await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");

        stop_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), lifecycle)
            .await
            .expect("lifecycle must finish after stop")
            .unwrap();
        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(
            TcpStream::connect(addr).await.is_err(),
            "listener still open after drain"
        );
    }

    #[tokio::test]
    async fn bind_failure_surfaces_as_error() {
        let occupied = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = occupied.local_addr().unwrap();

        let outcome = serve_until(addr, std::future::ready(())).await;
        assert!(
            matches!(outcome, Err(BootstrapError::Server(ServerError::Bind(_)))),
            "{outcome:?}"
        );
        drop(occupied);
    }
}
