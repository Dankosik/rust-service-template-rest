//! Background tasks the service owns from startup until the background-join
//! stage of teardown, and the capability that lends them to registrations.

use std::future::Future;

use infra_http::Server;
use tokio::{sync::watch, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackgroundFailure {
    pub(crate) name: &'static str,
    pub(crate) panicked: bool,
}

/// Startup-only access to the service's existing background task owner.
///
/// Bootstrap lends this non-cloneable capability while registering adapters.
/// It cannot change the root cancellation token, runtime or shutdown budget.
pub struct BackgroundRegistration<'a> {
    pub(super) background: &'a mut Background,
}

impl BackgroundRegistration<'_> {
    /// Register one process-lifetime manager before traffic admission.
    ///
    /// `start` must construct its future promptly. The child token fires in
    /// the existing background-join stage. On cancellation, close feature
    /// admission and wait for every admitted operation before returning `Ok`.
    /// On live work failure, close admission, call the reporter immediately,
    /// then retain completion ownership until all admitted work ends before
    /// returning `Err`. Reporting failure does not complete or detach the task.
    ///
    /// An early `Ok`, any panic, or any `Err` is a failure. Errors are never
    /// formatted here. A failure while serving stops the process with exit 1;
    /// a failure during requested cleanup makes its outcome degraded (exit 3).
    /// The service's existing join deadline bounds the wait, not the lifetime
    /// of a started blocking operation; expiry cannot certify its termination.
    pub fn spawn<F, E>(
        &mut self,
        name: &'static str,
        start: impl FnOnce(CancellationToken, BackgroundFailureReporter) -> F,
    ) where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: Send + 'static,
    {
        let reporter = BackgroundFailureReporter {
            name,
            failure: self.background.failure.clone(),
        };
        let future = start(self.background.child_token(), reporter);
        self.background.spawn_fallible(name, future);
    }
}

impl std::fmt::Debug for BackgroundRegistration<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundRegistration")
            .finish_non_exhaustive()
    }
}

/// A manager's static-name failure signal into the existing service latch.
///
/// Reporting starts the root's failure transition without ending the manager.
/// The manager still owns closing admission and joining all its admitted work.
pub struct BackgroundFailureReporter {
    name: &'static str,
    failure: watch::Sender<Option<BackgroundFailure>>,
}

impl BackgroundFailureReporter {
    /// Latch this manager's failure without exposing its error or ending it.
    /// Repeated reports cannot replace the first observed process failure.
    pub fn report(&self) {
        self.record(false);
    }

    fn record(&self, panicked: bool) {
        self.failure.send_if_modified(|observed| {
            if observed.is_none() {
                *observed = Some(BackgroundFailure {
                    name: self.name,
                    panicked,
                });
                true
            } else {
                false
            }
        });
    }
}

impl std::fmt::Debug for BackgroundFailureReporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundFailureReporter")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

struct Completion {
    reporter: BackgroundFailureReporter,
    cancel: CancellationToken,
    failed: bool,
}

impl Drop for Completion {
    fn drop(&mut self) {
        let panicked = std::thread::panicking();
        if self.failed || panicked || !self.cancel.is_cancelled() {
            tracing::error!(
                task = self.reporter.name,
                panicked,
                "background_task_stopped"
            );
            self.reporter.record(panicked);
        }
    }
}

/// Private composition owner; dependencies must outlive every registered future.
pub(crate) struct Background {
    tasks: JoinSet<()>,
    cancel: CancellationToken,
    failure: watch::Sender<Option<BackgroundFailure>>,
}

impl Background {
    pub(crate) fn new(cancel: CancellationToken) -> Self {
        let (failure, _) = watch::channel(None);
        Self {
            tasks: JoinSet::new(),
            cancel,
            failure,
        }
    }

    /// A token that fires when teardown cancels this owner's tasks.
    pub(crate) fn child_token(&self) -> CancellationToken {
        self.cancel.child_token()
    }

    /// Track a task until teardown joins it. Returning before
    /// [`Self::child_token`] fires, or panicking, is a failure of the service.
    pub(crate) fn spawn(
        &mut self,
        name: &'static str,
        future: impl Future<Output = ()> + Send + 'static,
    ) {
        self.spawn_fallible(name, async move {
            future.await;
            Ok::<(), std::convert::Infallible>(())
        });
    }

    fn spawn_fallible<E: Send + 'static>(
        &mut self,
        name: &'static str,
        future: impl Future<Output = Result<(), E>> + Send + 'static,
    ) {
        let guard = Completion {
            reporter: BackgroundFailureReporter {
                name,
                failure: self.failure.clone(),
            },
            cancel: self.cancel.clone(),
            failed: false,
        };
        self.tasks.spawn(async move {
            let mut guard = guard;
            guard.failed = future.await.is_err();
        });
    }

    pub(crate) fn observer(&self) -> watch::Receiver<Option<BackgroundFailure>> {
        self.failure.subscribe()
    }

    pub(crate) fn failed(&self) -> Option<BackgroundFailure> {
        *self.failure.borrow()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub(crate) fn watch_listener(&mut self, name: &'static str, listener: &Server) {
        let failure = listener.accept_failure();
        let cancel = self.child_token();
        self.spawn(name, async move {
            tokio::select! {
                biased;
                failure = failure => tracing::error!(listener = name, ?failure, "listener failed"),
                () = cancel.cancelled() => {},
            }
        });
    }

    /// Cancel every task's token; teardown then joins them.
    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(super) fn abort_all(&mut self) {
        self.tasks.abort_all();
    }

    pub(super) async fn join_until(&mut self, deadline: Instant) -> JoinOutcome {
        let mut failed = self.failed().is_some();
        loop {
            match tokio::time::timeout_at(deadline, self.tasks.join_next()).await {
                Ok(Some(Ok(()))) => {}
                Ok(Some(Err(error))) => {
                    failed = true;
                    tracing::warn!(
                        panicked = error.is_panic(),
                        cancelled = error.is_cancelled(),
                        "background_join_failed"
                    );
                }
                Ok(None) => {
                    return if failed || self.failed().is_some() {
                        JoinOutcome::Failed
                    } else {
                        JoinOutcome::Complete
                    };
                }
                Err(_) => return JoinOutcome::Unconfirmed,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JoinOutcome {
    Complete,
    Failed,
    Unconfirmed,
}

pub(crate) async fn background_failure(
    receiver: &mut watch::Receiver<Option<BackgroundFailure>>,
) -> BackgroundFailure {
    loop {
        if let Some(failure) = *receiver.borrow_and_update() {
            return failure;
        }
        if receiver.changed().await.is_err() {
            return BackgroundFailure {
                name: "background_owner",
                panicked: false,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::FutureExt;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn progress_observer_reports_without_readers_and_keeps_join_custody() {
        let policy = health::RefreshPolicy {
            interval: Duration::from_secs(1),
            probe_budget: Duration::from_secs(1),
            failure_threshold: 3,
        };
        let readiness = health::Readiness::new(Vec::new(), policy);
        readiness.refresh().await;
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let mut observer = background.observer();
        super::super::start_progress(&readiness, &mut background).unwrap();
        // Neither tracked future has run yet; both remain live across the gap.
        tokio::time::advance(policy.stale_after() + Duration::from_secs(1)).await;
        let failure =
            tokio::time::timeout(Duration::from_secs(1), background_failure(&mut observer))
                .await
                .unwrap();
        assert_eq!(failure.name, "readiness_progress");
        assert!(!failure.panicked);
        assert_eq!(background.tasks.len(), 2);
        assert!(
            background.tasks.join_next().now_or_never().is_none(),
            "reporting a failure must not end either process-lifetime task"
        );
        let _ = readiness.stop_progress();
        cancel.cancel();
        assert_eq!(
            background
                .join_until(Instant::now() + Duration::from_secs(1))
                .await,
            JoinOutcome::Failed
        );
        assert!(background.is_empty());
    }

    #[tokio::test]
    async fn task_failure_is_sticky_before_join() {
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let mut observed = background.observer();
        background.spawn("early", async {});
        let failure =
            tokio::time::timeout(Duration::from_secs(1), background_failure(&mut observed))
                .await
                .unwrap();
        assert_eq!(
            failure,
            BackgroundFailure {
                name: "early",
                panicked: false
            }
        );
        cancel.cancel();
        assert_eq!(
            background
                .join_until(Instant::now() + Duration::from_secs(1))
                .await,
            JoinOutcome::Failed
        );
    }

    #[tokio::test]
    async fn reported_failure_keeps_the_manager_owned_until_its_completion() {
        // No Debug or Display: the public capability accepts an opaque failure.
        struct PrivateFailure;

        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let mut observed = background.observer();
        let (reporter_tx, reporter_rx) = tokio::sync::oneshot::channel();
        let (release, finish) = tokio::sync::oneshot::channel();
        BackgroundRegistration {
            background: &mut background,
        }
        .spawn("feature_manager", move |stop, reporter| {
            reporter_tx.send(reporter).unwrap();
            async move {
                stop.cancelled().await;
                finish.await.unwrap();
                Err::<(), _>(PrivateFailure)
            }
        });
        let reporter = reporter_rx.await.unwrap();
        reporter.report();
        reporter.report();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), background_failure(&mut observed))
                .await
                .unwrap(),
            BackgroundFailure {
                name: "feature_manager",
                panicked: false,
            }
        );
        cancel.cancel();
        let joining = background.join_until(Instant::now() + Duration::from_secs(1));
        tokio::pin!(joining);
        assert!(futures_util::poll!(&mut joining).is_pending());
        release.send(()).unwrap();
        assert_eq!(joining.await, JoinOutcome::Failed);
    }
}
