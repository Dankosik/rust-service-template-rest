//! The process stop signals and the time the first one arrived.

use futures_util::FutureExt;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("stop signal receiver closed unexpectedly ({signal})")]
pub(crate) struct SignalError {
    signal: &'static str,
}

/// Native streams remain owned by the synchronous entrypoint through runtime shutdown.
pub(crate) struct Signals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    first_stop: Option<Instant>,
}

impl Signals {
    pub(crate) fn install() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let interrupt = signal(SignalKind::interrupt())?;
            let terminate = signal(SignalKind::terminate())?;
            Ok(Self {
                interrupt,
                terminate,
                first_stop: None,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                ctrl_c: tokio::signal::windows::ctrl_c()?,
                first_stop: None,
            })
        }
    }

    /// Consume one native notification and retain its first observation time.
    pub(crate) async fn wait(&mut self) -> Result<(), SignalError> {
        #[cfg(unix)]
        let (notification, name) = tokio::select! {
            signal = self.terminate.recv() => (signal, "SIGTERM"),
            signal = self.interrupt.recv() => (signal, "SIGINT"),
        };
        #[cfg(windows)]
        let (notification, name) = (self.ctrl_c.recv().await, "ctrl-c");
        notification.ok_or(SignalError { signal: name })?;
        self.first_stop.get_or_insert_with(Instant::now);
        tracing::info!(signal = name, "stop requested");
        Ok(())
    }

    /// Poll and consume only an already pending notification.
    pub(crate) fn take_pending(&mut self) -> Result<bool, SignalError> {
        match self.wait().now_or_never() {
            Some(result) => result.map(|()| true),
            None => Ok(false),
        }
    }

    pub(crate) fn first_stop(&self) -> Option<Instant> {
        self.first_stop
    }
}
