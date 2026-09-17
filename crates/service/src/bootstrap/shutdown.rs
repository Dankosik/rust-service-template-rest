//! Process stop signal.
//!
//! Resolves on `SIGINT` or `SIGTERM` (Unix) or Ctrl-C (other platforms).
//! Only the composition root awaits it; handlers never observe signals.

/// Resolve once the process is asked to stop.
pub(crate) async fn signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "install ctrl-c handler; stopping now");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(err) => {
                tracing::error!(error = %err, "install SIGTERM handler; stopping now");
                return;
            }
        };
        tokio::select! {
            () = ctrl_c => tracing::info!(signal = "SIGINT", "stop requested"),
            _ = terminate.recv() => tracing::info!(signal = "SIGTERM", "stop requested"),
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
        tracing::info!(signal = "ctrl-c", "stop requested");
    }
}
