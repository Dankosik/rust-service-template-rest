use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Clone)]
pub(crate) struct Background {
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    first_failure: watch::Sender<Option<&'static str>>,
    aborts: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Background {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            first_failure: watch::Sender::new(None),
            aborts: Arc::default(),
        }
    }

    pub(crate) fn spawn<F>(&self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = self.cancel.child_token();
        let task = start(cancel.clone());
        let guard = std::mem::DropGuard::new(
            (name, cancel, self.first_failure.clone()),
            |(name, cancel, first_failure)| {
                if cancel.is_cancelled() && !std::thread::panicking() {
                    return;
                }
                tracing::error!(
                    task = name,
                    panicked = std::thread::panicking(),
                    "background_task_stopped"
                );
                record_first(&first_failure, name);
            },
        );
        let task = self.tracker.spawn(async move {
            let _guard = guard;
            task.await;
        });
        self.aborts.lock().push(task.abort_handle());
    }

    pub(crate) fn failure(&self) -> Option<&'static str> {
        *self.first_failure.borrow()
    }

    pub(crate) fn record_failure(&self, name: &'static str) {
        record_first(&self.first_failure, name);
    }

    pub(crate) fn abort(&self) {
        for task in self.aborts.lock().iter() {
            task.abort();
        }
    }

    pub(crate) async fn wait_for_failure(&self) -> &'static str {
        let mut failures = self.first_failure.subscribe();
        if let Ok(seen) = failures.wait_for(Option::is_some).await
            && let Some(name) = *seen
        {
            return name;
        }
        std::future::pending().await
    }
}

fn record_first(failures: &watch::Sender<Option<&'static str>>, name: &'static str) {
    failures.send_if_modified(|first| {
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
            tokio::time::timeout(Duration::from_secs(3), background.wait_for_failure())
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
