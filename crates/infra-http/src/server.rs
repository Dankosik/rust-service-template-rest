//! HTTP server lifecycle: bind, serve, drain.
//!
//! The composition root decides *when* to stop; this module owns *how* the
//! listener stops accepting and in-flight requests are allowed to finish.

use std::future::IntoFuture;
use std::io;
use std::net::SocketAddr;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Server failures visible to the composition root.
#[derive(Debug)]
pub enum ServerError {
    /// The listener could not bind or report its address.
    Bind(io::Error),
    /// The accept loop failed after startup.
    Serve(io::Error),
    /// The serving task panicked or was cancelled.
    Task(tokio::task::JoinError),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(err) => write!(f, "bind http listener: {err}"),
            Self::Serve(err) => write!(f, "serve http: {err}"),
            Self::Task(err) => write!(f, "http server task: {err}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind(err) | Self::Serve(err) => Some(err),
            Self::Task(err) => Some(err),
        }
    }
}

/// A bound and serving HTTP server.
///
/// Dropping the value without calling [`Server::shutdown`] aborts the accept
/// loop without draining; callers that own a lifecycle should always drain.
#[derive(Debug)]
pub struct Server {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<io::Result<()>>,
}

impl Server {
    /// Bind `addr` and start serving `app` on the current Tokio runtime.
    ///
    /// Returns once the listener is bound, so callers can flip readiness and
    /// log the resolved address before any request arrives.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Bind`] when the address cannot be bound.
    pub async fn bind(addr: SocketAddr, app: Router) -> Result<Self, ServerError> {
        let listener = TcpListener::bind(addr).await.map_err(ServerError::Bind)?;
        let local_addr = listener.local_addr().map_err(ServerError::Bind)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let serve = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // A dropped sender also stops the server; both paths drain.
                let _ = shutdown_rx.await;
            })
            .into_future();
        let task = tokio::spawn(serve);
        Ok(Self {
            local_addr,
            shutdown: Some(shutdown_tx),
            task,
        })
    }

    /// The address the listener actually bound, including an OS-assigned port.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting connections, let in-flight requests finish, and wait
    /// for the accept loop to exit.
    ///
    /// The caller bounds this with its own shutdown budget; the server does
    /// not choose a timeout.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Serve`] when the accept loop failed or
    /// [`ServerError::Task`] when the serving task did not complete normally.
    pub async fn shutdown(mut self) -> Result<(), ServerError> {
        if let Some(shutdown) = self.shutdown.take() {
            // The receiver is only gone if the server already stopped.
            let _ = shutdown.send(());
        }
        match (&mut self.task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(ServerError::Serve(err)),
            Err(err) => Err(ServerError::Task(err)),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Reached only when `shutdown` was not awaited; do not leak the task.
        if self.shutdown.is_some() {
            self.task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::{Readiness, router};

    fn loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

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
    async fn binds_an_ephemeral_port_and_serves_probes() {
        let readiness = Readiness::new();
        let server = Server::bind(loopback(), router(readiness.clone()))
            .await
            .unwrap();
        assert_ne!(server.local_addr().port(), 0);

        let response = get(server.local_addr(), "/health/ready").await;
        assert!(response.starts_with("HTTP/1.1 503 "), "{response}");

        readiness.set_ready(true);
        let response = get(server.local_addr(), "/health/ready").await;
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");

        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_completes_and_releases_the_port() {
        let server = Server::bind(loopback(), router(Readiness::new()))
            .await
            .unwrap();
        let addr = server.local_addr();

        tokio::time::timeout(Duration::from_secs(5), server.shutdown())
            .await
            .expect("shutdown must complete without in-flight requests")
            .unwrap();

        let refused = TcpStream::connect(addr).await;
        assert!(refused.is_err(), "listener still accepting after shutdown");
    }

    #[tokio::test]
    async fn bind_failure_is_reported_not_panicked() {
        let occupied = Server::bind(loopback(), router(Readiness::new()))
            .await
            .unwrap();
        let err = Server::bind(occupied.local_addr(), router(Readiness::new()))
            .await
            .expect_err("second bind to the same port must fail");
        assert!(matches!(err, ServerError::Bind(_)), "{err}");
        occupied.shutdown().await.unwrap();
    }
}
