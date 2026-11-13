//! Background tasks the worker owns until the background-join stage.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The worker's background tasks and their root cancellation token.
///
/// Every task runs until its token is cancelled, and nothing cancels before
/// teardown, so a task that ends earlier is a panic or a defect: the worker
/// stops rather than run without it, as the service does. A task spawned
/// through [`Self::spawn`] reports that end by name. The engines spawn their
/// loops on [`Self::tracker`] and report through `Started::failed`.
#[derive(Clone)]
pub(crate) struct Background {
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    stopped: watch::Sender<Option<&'static str>>,
    aborts: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Background {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            stopped: watch::Sender::new(None),
            aborts: Arc::default(),
        }
    }

    /// Spawn the task `start` builds from a child of the root token.
    pub(crate) fn spawn<F>(&self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = self.cancel.child_token();
        let task = start(cancel.clone());
        let guard = ReportUnexpectedTaskEnd {
            name,
            cancel,
            stopped: self.stopped.clone(),
        };
        let task = self.tracker.spawn(async move {
            let _guard = guard;
            task.await;
        });
        self.aborts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task.abort_handle());
    }

    pub(crate) fn failure(&self) -> Option<&'static str> {
        *self.stopped.borrow()
    }

    /// Report a live task's failure while retaining its completion custody.
    pub(crate) fn record_failure(&self, name: &'static str) {
        record_first(&self.stopped, name);
    }

    pub(crate) fn abort(&self) {
        for task in self
            .aborts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            task.abort();
        }
    }

    /// Wait for the first recorded failure, including one reported by a task
    /// that is still running.
    pub(crate) async fn stopped(&self) -> &'static str {
        let mut stopped = self.stopped.subscribe();
        if let Ok(seen) = stopped.wait_for(Option::is_some).await
            && let Some(name) = *seen
        {
            return name;
        }
        // `self` owns the sender, so the receiver cannot close while `self`
        // is borrowed; this only keeps the function total.
        std::future::pending().await
    }
}

struct ReportUnexpectedTaskEnd {
    name: &'static str,
    cancel: CancellationToken,
    stopped: watch::Sender<Option<&'static str>>,
}

impl Drop for ReportUnexpectedTaskEnd {
    fn drop(&mut self) {
        if self.cancel.is_cancelled() && !std::thread::panicking() {
            return;
        }
        tracing::error!(
            task = self.name,
            panicked = std::thread::panicking(),
            "background_task_stopped"
        );
        record_first(&self.stopped, self.name);
    }
}

/// The first reported task names the failure; later ones only log.
fn record_first(stopped: &watch::Sender<Option<&'static str>>, name: &'static str) {
    stopped.send_if_modified(|first| {
        if first.is_some() {
            return false;
        }
        *first = Some(name);
        true
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Background;

    #[tokio::test]
    async fn a_task_that_panics_is_reported_by_name() {
        let background = Background::new();
        background.spawn("panics", |_cancel| async { panic!("task defect") });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), background.stopped())
                .await
                .unwrap(),
            "panics"
        );
        background.tracker.close();
        tokio::time::timeout(Duration::from_secs(3), background.tracker.wait())
            .await
            .unwrap();
        background.spawn("later", |_cancel| async {});
        tokio::time::timeout(Duration::from_secs(3), background.tracker.wait())
            .await
            .unwrap();
        assert_eq!(background.failure(), Some("panics"), "the first end stands");
    }
}
