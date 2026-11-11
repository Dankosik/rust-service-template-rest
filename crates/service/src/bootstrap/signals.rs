//! Stop signals, owned through the runtime's final shutdown.

use futures_util::FutureExt;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("stop signal receiver closed unexpectedly")]
pub(crate) struct SignalError;

/// Native streams stay owned through the runtime's final shutdown.
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
                terminate,
                interrupt,
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

    pub(crate) fn first_stop(&self) -> Option<Instant> {
        self.first_stop
    }

    /// Consume one native notification and retain its first observation time.
    pub(crate) async fn wait(&mut self) -> Result<(), SignalError> {
        #[cfg(unix)]
        let (received, name) = tokio::select! {
            received = self.terminate.recv() => (received, "SIGTERM"),
            received = self.interrupt.recv() => (received, "SIGINT"),
        };
        #[cfg(windows)]
        let (received, name) = (self.ctrl_c.recv().await, "ctrl-c");
        received.ok_or(SignalError)?;
        self.first_stop.get_or_insert_with(Instant::now);
        tracing::info!(signal = name, "stop requested");
        Ok(())
    }

    /// Poll and consume only an already pending notification.
    pub(crate) fn pending(&mut self) -> Result<bool, SignalError> {
        self.wait()
            .now_or_never()
            .transpose()
            .map(|value| value.is_some())
    }
}
