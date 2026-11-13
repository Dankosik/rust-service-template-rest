use std::fmt;
use std::sync::{Arc, Mutex};

use crate::{Code, Status};

/// Request extension set by the dispatcher when it can report the decoded outcome.
#[derive(Clone, Copy, Debug)]
pub struct TrackResponseStatus;

/// Response extension for reporting the final code to the underlying service.
///
/// The dispatcher takes the callback when it can report the result. A local
/// transport stop takes precedence over any buffered response.
#[derive(Clone)]
pub struct ResponseStatusObserver {
    completion: Arc<Mutex<Completion>>,
}

type Notify = Box<dyn FnOnce(Code) + Send>;

struct Completion {
    notify: Option<Notify>,
    stop: Option<Status>,
}

impl ResponseStatusObserver {
    /// Creates a one-shot observer for a response consumed by the dispatcher.
    pub fn new(notify: impl FnOnce(Code) + Send + 'static) -> Self {
        Self {
            completion: Arc::new(Mutex::new(Completion {
                notify: Some(Box::new(notify)),
                stop: None,
            })),
        }
    }

    /// Records a local stop before exposing the terminated transport state.
    pub fn stop(&self, status: &Status) {
        let mut completion = self
            .completion
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if completion.notify.is_some() && completion.stop.is_none() {
            completion.stop = Some(status.clone());
        }
    }

    /// Emits the recorded stop even if the dispatcher never polls again.
    pub fn finish_stop(&self) {
        if let Some(status) = self.stopped() {
            let _ = self.finish::<()>(Err(status));
        }
    }

    pub(crate) fn stopped(&self) -> Option<Status> {
        self.completion
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stop
            .clone()
    }

    pub(crate) fn finish<T>(&self, result: Result<T, Status>) -> Result<T, Status> {
        let mut completion = self
            .completion
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let stop = completion.stop.clone();
        let notify = completion.notify.take();
        drop(completion);
        let result = if let Some(status) = stop {
            Err(status)
        } else {
            result
        };
        if let Some(notify) = notify {
            notify(
                result
                    .as_ref()
                    .map_or_else(|status| status.code(), |_| Code::Ok),
            );
        }
        result
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(notify) = self.notify.take() {
            notify(self.stop.as_ref().map_or(Code::Cancelled, Status::code));
        }
    }
}

impl fmt::Debug for ResponseStatusObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseStatusObserver")
            .finish_non_exhaustive()
    }
}
