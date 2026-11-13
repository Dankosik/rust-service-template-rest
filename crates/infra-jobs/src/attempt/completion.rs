//! Completion writes for one fenced claim or a batch sharing a supervisor deadline.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use infra_postgres::observed;
use tokio::sync::oneshot::{self, error::TryRecvError};
use tokio::time::Instant;

use super::outcome::{Unacknowledged, Written};
use super::{
    AttemptSlots, COMPLETION_MEMBERSHIPS_METRIC, ClaimFence, MembershipGauge, MetricMembership,
    deadline_reached_or_cleanup_ended, within,
};
use crate::engine::{Operation, OperationError, Shared, backstop, observe_failure};

type CompletionReply = Result<Written, Unacknowledged>;

pub(crate) struct Completions {
    groups: Mutex<BTreeMap<Instant, Weak<CompletionGroup>>>,
    pub(crate) memberships: MembershipGauge,
}

impl Default for Completions {
    fn default() -> Self {
        Self {
            groups: Mutex::default(),
            memberships: MembershipGauge::new(COMPLETION_MEMBERSHIPS_METRIC),
        }
    }
}

impl Completions {
    fn group(&self, deadline: Instant) -> Arc<CompletionGroup> {
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(group) = groups.get(&deadline).and_then(Weak::upgrade) {
            return group;
        }
        groups.retain(|_, group| group.strong_count() != 0);
        let group = Arc::new(CompletionGroup::default());
        groups.insert(deadline, Arc::downgrade(&group));
        group
    }
}

/// One writer per deadline. No writer can hold another deadline's queue.
#[derive(Default)]
struct CompletionGroup {
    queued: Mutex<Vec<QueuedCompletion>>,
    writer: tokio::sync::Mutex<()>,
}

enum WriteTurn<'a> {
    Write(tokio::sync::MutexGuard<'a, ()>),
    Reply(CompletionReply),
}

struct QueuedCompletion {
    fence: ClaimFence,
    // Fields drop in declaration order: the membership retires before reply
    // closure can wake a waiter that immediately registers another completion.
    membership: MetricMembership,
    slots: Arc<AttemptSlots>,
    reply: oneshot::Sender<CompletionReply>,
}

impl QueuedCompletion {
    fn retire(self) -> oneshot::Sender<CompletionReply> {
        let Self {
            membership,
            slots,
            reply,
            ..
        } = self;
        drop(membership);
        drop(slots);
        reply
    }
}

/// Dropping a waiting supervisor removes only its own queued registration.
/// Once taken by the writer, the batch owns retirement instead.
struct CompletionRegistration<'a> {
    group: &'a CompletionGroup,
    fence: ClaimFence,
}

impl Drop for CompletionRegistration<'_> {
    fn drop(&mut self) {
        let retired = {
            let mut queued = self.group.lock_queue();
            queued
                .iter()
                .position(|entry| entry.fence == self.fence)
                .map(|index| queued.swap_remove(index))
        };
        drop(retired);
    }
}

impl CompletionGroup {
    fn register(
        &self,
        fence: ClaimFence,
        slots: &Arc<AttemptSlots>,
        memberships: &MembershipGauge,
    ) -> (
        CompletionRegistration<'_>,
        oneshot::Receiver<CompletionReply>,
    ) {
        let (reply, response) = oneshot::channel();
        let registration = CompletionRegistration { group: self, fence };
        let queued = QueuedCompletion {
            fence,
            membership: memberships.join(),
            slots: Arc::clone(slots),
            reply,
        };
        self.lock_queue().push(queued);
        (registration, response)
    }

    fn lock_queue(&self) -> MutexGuard<'_, Vec<QueuedCompletion>> {
        self.queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn writer_or_reply(
        &self,
        response: &mut oneshot::Receiver<CompletionReply>,
    ) -> WriteTurn<'_> {
        let writer = tokio::select! {
            biased;
            reply = &mut *response => return WriteTurn::Reply(reply.unwrap_or(Err(Unacknowledged))),
            writer = self.writer.lock() => writer,
        };
        // The previous writer may have sent our reply between the two polls.
        match response.try_recv() {
            Ok(reply) => WriteTurn::Reply(reply),
            Err(TryRecvError::Closed) => WriteTurn::Reply(Err(Unacknowledged)),
            Err(TryRecvError::Empty) => WriteTurn::Write(writer),
        }
    }
}

/// Retire the entire batch before a reply or sender closure can wake a waiter.
struct CompletionBatch {
    queued: Vec<QueuedCompletion>,
    pruned_replies: Vec<oneshot::Sender<CompletionReply>>,
}

impl CompletionBatch {
    fn new(queued: Vec<QueuedCompletion>) -> Self {
        Self {
            queued,
            pruned_replies: Vec::new(),
        }
    }

    /// Retire the entries `stale` selects, keeping the order of the rest. Their
    /// senders stay open until the batch finishes or drops.
    fn prune(&mut self, stale: impl FnMut(&mut QueuedCompletion) -> bool) {
        self.pruned_replies.extend(
            self.queued
                .extract_if(.., stale)
                .map(QueuedCompletion::retire),
        );
    }

    fn retire(&mut self) -> Vec<oneshot::Sender<CompletionReply>> {
        std::mem::take(&mut self.queued)
            .into_iter()
            .map(QueuedCompletion::retire)
            .collect()
    }

    fn finish(mut self, results: Vec<CompletionReply>) {
        let replies: Vec<_> = self.retire().into_iter().zip(results).collect();
        // Also close pruned replies only after every entry and SQL buffer is gone.
        drop(self);
        for (reply, result) in replies {
            let _ = reply.send(result);
        }
    }
}

impl Drop for CompletionBatch {
    fn drop(&mut self) {
        drop(self.retire());
    }
}

/// COMPLETE for one fenced claim, for the caller to execute.
///
/// Every outcome write fences on `claim_expires_at IS NOT NULL`, which the
/// table's CHECK makes equivalent to `state = 'running'`. A literal
/// `state = 'running'` proves the partial `background_jobs_running` predicate,
/// and after ANALYZE saw few running rows the planner scans that whole index
/// for the id, dead entries of every job claimed since the last VACUUM
/// included. This predicate leaves the primary key as the only access path.
pub(crate) fn complete_query(
    fence: ClaimFence,
) -> sqlx::query::Query<'static, sqlx::Postgres, sqlx::postgres::PgArguments> {
    sqlx::query!(
        "UPDATE background_jobs \
         SET state = 'completed', finished_at = statement_timestamp(), claim_expires_at = NULL \
         WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
        fence.job_id.0,
        fence.generation,
    )
}

pub(super) async fn complete_batched(
    shared: &Shared,
    fence: ClaimFence,
    supervisor_deadline: Instant,
    slots: &Arc<AttemptSlots>,
) -> CompletionReply {
    let group = shared.completions.group(supervisor_deadline);
    let (_registration, mut response) =
        group.register(fence, slots, &shared.completions.memberships);
    let writer = match group.writer_or_reply(&mut response).await {
        WriteTurn::Write(writer) => writer,
        WriteTurn::Reply(reply) => return reply,
    };
    let batch = CompletionBatch::new(std::mem::take(&mut *group.lock_queue()));
    write_batch(shared, batch, supervisor_deadline).await;
    drop(writer);
    response.await.unwrap_or(Err(Unacknowledged))
}

async fn write_batch(shared: &Shared, mut batch: CompletionBatch, deadline: Instant) {
    batch.prune(|entry| entry.reply.is_closed());
    if batch.queued.is_empty() || deadline_reached_or_cleanup_ended(&shared.cleanup, deadline) {
        return;
    }
    // The batch's bind arrays live in this scope so the statement future ends,
    // and its borrow of them with it, before any reply wakes a waiter.
    let result = {
        let (ids, generations): (Vec<_>, Vec<_>) = batch
            .queued
            .iter()
            .map(|queued| (queued.fence.job_id.0, queued.fence.generation))
            .unzip();
        within(
            &shared.cleanup,
            deadline,
            backstop(Box::pin(async {
                let mut connection = infra_postgres::acquire(&shared.pool, "complete jobs batch")
                    .await
                    .map_err(OperationError::Acquire)?;
                if deadline_reached_or_cleanup_ended(&shared.cleanup, deadline) {
                    return Err(OperationError::TimedOut);
                }
                // COMPLETE for every attempt queued while the previous batch was in
                // flight, under the fence `complete_query` explains. Returns the 1-based
                // position of each applied completion.
                observed(
                    "complete jobs",
                    sqlx::query_scalar!(
                        "UPDATE background_jobs AS job \
                         SET state = 'completed', finished_at = statement_timestamp(), claim_expires_at = NULL \
                         FROM unnest($1::uuid[], $2::bigint[]) WITH ORDINALITY AS done (id, generation, position) \
                         WHERE job.id = done.id AND job.claim_generation = done.generation \
                           AND job.claim_expires_at IS NOT NULL \
                         RETURNING done.position AS \"position!\"",
                        &ids,
                        &generations,
                    )
                    .fetch_all(&mut *connection),
                )
                .await
                .map_err(OperationError::from)
            })),
        )
        .await
    };
    match result {
        Some(Ok(positions)) => {
            let results = written_by_position(&positions, batch.queued.len())
                .into_iter()
                .map(Ok)
                .collect();
            batch.finish(results);
        }
        Some(Err(error)) => {
            observe_failure(shared, Operation::Record, &error);
            drop(error);
            let results = vec![Err(Unacknowledged); batch.queued.len()];
            batch.finish(results);
        }
        None => {}
    }
}

/// Each of `len` batched completions' writes, from the 1-based positions the
/// statement applied.
fn written_by_position(positions: &[i64], len: usize) -> Vec<Written> {
    let mut written = vec![Written::Unchanged; len];
    for &position in positions {
        if let Some(entry) = usize::try_from(position - 1)
            .ok()
            .and_then(|index| written.get_mut(index))
        {
            *entry = Written::Applied;
        }
    }
    written
}
#[cfg(test)]
mod tests {
    use std::future::Future as _;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::*;
    use crate::attempt::{OWNED_ATTEMPTS_METRIC, describe_metrics};
    use crate::gauge_recorder::Gauges;
    use crate::kind::JobId;

    fn admitted(slots: &Arc<tokio::sync::Semaphore>) -> Arc<AttemptSlots> {
        Arc::new(AttemptSlots {
            _membership: MembershipGauge::owned_attempts().join(),
            _global: Arc::clone(slots).try_acquire_owned().unwrap(),
            _kind: None,
        })
    }

    const fn fence(generation: i64) -> ClaimFence {
        ClaimFence {
            job_id: JobId(uuid::Uuid::nil()),
            generation,
        }
    }

    #[tokio::test]
    async fn an_earlier_deadline_does_not_block_a_later_writer() {
        let completions = Completions::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        let earlier = completions.group(deadline);
        let later = completions.group(deadline + Duration::from_millis(100));
        let _earlier_writer = earlier.writer.lock().await;
        let (_reply, mut response) = oneshot::channel();
        let mut turn = std::pin::pin!(later.writer_or_reply(&mut response));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            turn.as_mut().poll(&mut context),
            std::task::Poll::Ready(WriteTurn::Write(_))
        ));
    }

    #[tokio::test]
    async fn an_acknowledged_attempt_can_finish_while_a_writer_holds_the_turn() {
        let group = CompletionGroup::default();
        let writer = group.writer.lock().await;
        let (reply, mut response) = oneshot::channel();
        let mut turn = std::pin::pin!(group.writer_or_reply(&mut response));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(turn.as_mut().poll(&mut context).is_pending());
        reply.send(Ok(Written::Applied)).unwrap();
        assert!(matches!(
            turn.as_mut().poll(&mut context),
            std::task::Poll::Ready(WriteTurn::Reply(Ok(Written::Applied)))
        ));
        drop(writer);
    }

    #[test]
    fn cancelled_queued_registration_retires_only_its_claim_generation() {
        let gauges = Gauges::default();
        metrics::with_local_recorder(&gauges, || {
            let completions = Completions::default();
            let capacity = Arc::new(tokio::sync::Semaphore::new(2));
            let first = admitted(&capacity);
            let second = admitted(&capacity);
            let deadline = Instant::now() + Duration::from_secs(10);
            let group = completions.group(deadline);
            let (first_registration, mut first_reply) =
                group.register(fence(1), &first, &completions.memberships);
            let (second_registration, mut second_reply) =
                group.register(fence(2), &second, &completions.memberships);
            drop(first);
            drop(second);
            assert_eq!(capacity.available_permits(), 0);
            assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 2.0);
            assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 2.0);
            describe_metrics();
            assert_eq!(
                gauges.get(OWNED_ATTEMPTS_METRIC, None),
                2.0,
                "another engine startup must not reset live memberships"
            );

            drop(first_registration);
            assert_eq!(capacity.available_permits(), 1);
            assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 1.0);
            assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 1.0);
            assert_eq!(group.lock_queue().len(), 1);
            std::assert_matches!(
                first_reply.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            );
            std::assert_matches!(
                second_reply.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            );
            drop(second_registration);
            assert_eq!(capacity.available_permits(), 2);
            assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 0.0);
            assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 0.0);
            assert!(group.lock_queue().is_empty());
        });
    }

    struct ReplyWake {
        capacity: Arc<tokio::sync::Semaphore>,
        available_at_wake: std::sync::atomic::AtomicUsize,
    }

    impl std::task::Wake for ReplyWake {
        fn wake(self: Arc<Self>) {
            self.available_at_wake
                .store(self.capacity.available_permits(), Ordering::SeqCst);
        }
    }

    #[test]
    fn entire_batch_retires_before_success_failure_cancellation_or_pruning_wakes_a_waiter() {
        let gauges = Gauges::default();
        metrics::with_local_recorder(&gauges, || {
            for result in [
                Some(Ok(Written::Applied)),
                Some(Ok(Written::Unchanged)),
                Some(Err(Unacknowledged)),
                None,
            ] {
                for prune in [false, true] {
                    let completions = Completions::default();
                    let group = completions.group(Instant::now() + Duration::from_secs(10));
                    let capacity = Arc::new(tokio::sync::Semaphore::new(4));
                    let mut registrations = Vec::new();
                    let mut responses = Vec::new();
                    let mut observers = Vec::new();
                    for generation in 1..=4 {
                        let slots = admitted(&capacity);
                        let (registration, mut response) =
                            group.register(fence(generation), &slots, &completions.memberships);
                        let wake = Arc::new(ReplyWake {
                            capacity: Arc::clone(&capacity),
                            available_at_wake: std::sync::atomic::AtomicUsize::new(usize::MAX),
                        });
                        let waker = std::task::Waker::from(Arc::clone(&wake));
                        let mut context = std::task::Context::from_waker(&waker);
                        assert!(
                            std::pin::Pin::new(&mut response)
                                .poll(&mut context)
                                .is_pending()
                        );
                        registrations.push(registration);
                        responses.push(response);
                        observers.push(wake);
                    }
                    let mut batch = CompletionBatch::new(std::mem::take(&mut *group.lock_queue()));
                    drop(registrations);
                    assert_eq!(capacity.available_permits(), 0, "batch retains admission");
                    assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 4.0);
                    assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 4.0);

                    if prune {
                        batch.prune(|entry| entry.fence.generation == 1);
                        std::assert_matches!(
                            responses[0].try_recv(),
                            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                        );
                        assert_eq!(
                            observers[0].available_at_wake.load(Ordering::SeqCst),
                            usize::MAX,
                            "a pruned sender stays open until the remaining batch retires"
                        );
                    }
                    match result {
                        Some(result) => {
                            let results = vec![result; batch.queued.len()];
                            batch.finish(results);
                        }
                        None => drop(batch),
                    }
                    assert_eq!(capacity.available_permits(), 4);
                    assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 0.0);
                    assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 0.0);
                    for (index, (wake, mut response)) in
                        observers.into_iter().zip(responses).enumerate()
                    {
                        assert_eq!(wake.available_at_wake.load(Ordering::SeqCst), 4);
                        match result.filter(|_| !prune || index != 0) {
                            Some(result) => assert_eq!(response.try_recv().unwrap(), result),
                            None => std::assert_matches!(
                                response.try_recv(),
                                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
                            ),
                        }
                    }
                }
            }
        });
    }
}
