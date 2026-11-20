use std::time::Duration;

use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub(crate) struct Background {
    cancel: CancellationToken,
    tracker: TaskTracker,
    first_failure: watch::Sender<Option<&'static str>>,
    aborts: Vec<AbortHandle>,
}

impl Background {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            first_failure: watch::Sender::new(None),
            aborts: Vec::new(),
        }
    }

    // template:begin messaging:worker-background-cancel
    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
    // template:end messaging:worker-background-cancel

    pub(crate) fn child_token(&self) -> CancellationToken {
        self.cancel.child_token()
    }

    // template:begin jobs:worker-background-start-engine
    pub(crate) fn start_engine(&self, engine: infra_jobs::Engine) -> infra_jobs::Started {
        engine.start(&self.tracker, &self.cancel)
    }
    // template:end jobs:worker-background-start-engine

    pub(crate) fn spawn<F>(
        &mut self,
        name: &'static str,
        start: impl FnOnce(CancellationToken) -> F,
    ) where
        F: Future<Output = ()> + Send + 'static,
    {
        let owner_cancel = self.cancel.clone();
        let task = start(owner_cancel.child_token());
        let guard = std::mem::DropGuard::new(
            (name, owner_cancel, self.first_failure.clone()),
            |(name, owner_cancel, first_failure)| {
                if owner_cancel.is_cancelled() && !std::thread::panicking() {
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
        let abort = self
            .tracker
            .spawn(async move {
                let _guard = guard;
                task.await;
            })
            .abort_handle();
        self.aborts.push(abort);
    }

    pub(crate) fn failure(&self) -> Option<&'static str> {
        *self.first_failure.borrow()
    }

    pub(crate) fn failure_observer(&self) -> watch::Receiver<Option<&'static str>> {
        self.first_failure.subscribe()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tracker.is_empty()
    }

    pub(crate) async fn join_within(&self, budget: Duration) -> bool {
        self.cancel.cancel();
        self.tracker.close();
        tokio::time::timeout(budget, self.tracker.wait())
            .await
            .is_ok()
            && self.failure().is_none()
    }

    pub(crate) fn abort(&self) {
        for task in &self.aborts {
            task.abort();
        }
    }

    pub(crate) async fn wait_until(&self, deadline: Instant) -> bool {
        tokio::time::timeout_at(deadline, self.tracker.wait())
            .await
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    #[cfg(test)]
    pub(crate) fn is_closed(&self) -> bool {
        self.tracker.is_closed()
    }

    #[cfg(test)]
    pub(crate) fn tracker(&self) -> TaskTracker {
        self.tracker.clone()
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn close_and_wait(&self) {
        self.cancel.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}

pub(crate) async fn wait_for_failure(
    failures: &mut watch::Receiver<Option<&'static str>>,
) -> &'static str {
    if let Ok(seen) = failures.wait_for(Option::is_some).await
        && let Some(name) = *seen
    {
        return name;
    }
    std::future::pending().await
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

    use super::{Background, wait_for_failure};

    #[tokio::test]
    async fn a_task_that_panics_is_reported_by_name() {
        let mut background = Background::new();
        let mut failures = background.failure_observer();
        background.spawn("panics", |_cancel| async { panic!("task defect") });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), wait_for_failure(&mut failures))
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
