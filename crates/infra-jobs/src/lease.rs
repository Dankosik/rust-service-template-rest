//! Claim upkeep, release, and reconciliation.

use infra_postgres::{connection, in_tx_with};
use sqlx::Row;
use tokio::sync::SemaphorePermit;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::engine::{
    CANCEL_MARGIN, CLAIM_TTL, OpFailed, Operation, OperationError, READ_COMMITTED, Shared,
    backstop, observe_failure, observe_recovery,
};
use crate::kind::JobId;

/// Extend every still-current claim. Results are matched by the whole claim.
const EXTEND: &str = "UPDATE background_jobs AS job \
     SET claim_expires_at = statement_timestamp() + $3 \
     FROM unnest($1::uuid[], $2::bigint[]) AS claim (id, generation) \
     WHERE job.id = claim.id \
       AND job.claim_generation = claim.generation \
       AND job.state = 'running' \
     RETURNING job.id::text AS id, job.claim_generation";

/// Give the budget unit back and return the claim to pending.
const RELEASE: &str = "UPDATE background_jobs AS job \
     SET state = 'pending', attempts = job.attempts - 1, claim_expires_at = NULL \
     FROM unnest($1::uuid[], $2::bigint[]) AS claim (id, generation) \
     WHERE job.id = claim.id \
       AND job.claim_generation = claim.generation \
       AND job.state = 'running' \
     RETURNING job.id::text AS id, job.claim_generation";

/// The current row of each id. An id with no row is missing from the result.
const RECONCILE: &str = "SELECT id::text AS id, claim_generation, state, attempts \
     FROM background_jobs \
     WHERE id = ANY ($1::uuid[]) \
     FOR SHARE";

/// `sent + (CLAIM_TTL - CANCEL_MARGIN)`: 28 s after the statement was sent.
#[must_use]
pub(crate) fn local_deadline(sent: Instant) -> Instant {
    let after = CLAIM_TTL.saturating_sub(CANCEL_MARGIN);
    sent.checked_add(after).unwrap_or(sent)
}

/// Extend claims that are still running a handler, until `cancel` fires.
pub(crate) async fn run_upkeep(shared: std::sync::Arc<Shared>, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        let mut ticker = tokio::time::interval(crate::UPKEEP_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let claims = shared.attempts.extending();
            if claims.is_empty() {
                continue;
            }
            extend_claims(&shared, &claims).await;
        }
    }))
    .await;
}

async fn extend_claims(shared: &Shared, claims: &[(JobId, i64)]) {
    let Ok(permit) = engine_permit(shared).await else {
        return;
    };
    let (id_list, generations) = bind_claims(claims);
    let returned = std::sync::atomic::AtomicBool::new(false);
    let result = backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<(Instant, Vec<(JobId, i64)>), OpFailed> {
                let conn = connection(tx);
                let sent = Instant::now();
                let rows = sqlx::query(EXTEND)
                    .bind(&id_list)
                    .bind(&generations)
                    .bind(CLAIM_TTL)
                    .fetch_all(&mut *conn)
                    .await?;
                let decoded = decode_pairs(&rows)?;
                returned.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok((sent, decoded))
            },
        ),
    )
    .await;
    drop(permit);
    match result {
        Ok((sent, current)) => {
            observe_recovery(shared, Operation::Extend);
            apply_extend(shared, claims, sent, &current);
        }
        Err(error) => observe_failure(shared, Operation::Extend, error),
    }
}

fn apply_extend(shared: &Shared, claims: &[(JobId, i64)], sent: Instant, current: &[(JobId, i64)]) {
    for (id, generation) in claims {
        if current
            .iter()
            .any(|(found_id, found_generation)| found_id == id && found_generation == generation)
        {
            shared.attempts.acknowledge(*id, *generation, sent);
        } else {
            shared.attempts.supersede(*id, *generation);
        }
    }
}

/// The claims RELEASE returned. The caller holds the engine permit.
///
/// # Errors
///
/// [`OperationError`] when the statement does not finish as a known commit.
pub(crate) async fn release(
    shared: &Shared,
    claims: &[(JobId, i64)],
) -> Result<Vec<(JobId, i64)>, OperationError> {
    let (id_list, generations) = bind_claims(claims);
    let returned = std::sync::atomic::AtomicBool::new(false);
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<Vec<(JobId, i64)>, OpFailed> {
                let conn = connection(tx);
                let rows = sqlx::query(RELEASE)
                    .bind(&id_list)
                    .bind(&generations)
                    .fetch_all(&mut *conn)
                    .await?;
                let decoded = decode_pairs(&rows)?;
                returned.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(decoded)
            },
        ),
    )
    .await
}

/// The current row of each id. The caller holds any permit this statement needs.
///
/// # Errors
///
/// [`OperationError`] when the statement does not finish as a known commit.
pub(crate) async fn reconcile(
    shared: &Shared,
    ids: &[JobId],
) -> Result<Vec<RowState>, OperationError> {
    let id_list: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let returned = std::sync::atomic::AtomicBool::new(false);
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<Vec<RowState>, OpFailed> {
                let conn = connection(tx);
                let rows = sqlx::query(RECONCILE)
                    .bind(&id_list)
                    .fetch_all(&mut *conn)
                    .await?;
                let decoded = decode_states(&rows)?;
                returned.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(decoded)
            },
        ),
    )
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobState {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowState {
    pub(crate) id: JobId,
    pub(crate) generation: i64,
    pub(crate) state: JobState,
    pub(crate) attempts: i16,
}

fn bind_claims(claims: &[(JobId, i64)]) -> (Vec<String>, Vec<i64>) {
    let id_list = claims.iter().map(|(id, _)| id.to_string()).collect();
    let generations = claims.iter().map(|(_, generation)| *generation).collect();
    (id_list, generations)
}

fn decode_pairs(rows: &[sqlx::postgres::PgRow]) -> Result<Vec<(JobId, i64)>, OpFailed> {
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        decoded.push(decode_pair(row)?);
    }
    Ok(decoded)
}

fn decode_pair(row: &sqlx::postgres::PgRow) -> Result<(JobId, i64), OpFailed> {
    let id = job_id(row)?;
    let generation: i64 = row.try_get("claim_generation")?;
    Ok((id, generation))
}

fn decode_states(rows: &[sqlx::postgres::PgRow]) -> Result<Vec<RowState>, OpFailed> {
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        decoded.push(decode_state(row)?);
    }
    Ok(decoded)
}

fn decode_state(row: &sqlx::postgres::PgRow) -> Result<RowState, OpFailed> {
    let id = job_id(row)?;
    let generation: i64 = row.try_get("claim_generation")?;
    let state_text: String = row.try_get("state")?;
    let state = match state_text.as_str() {
        "pending" => JobState::Pending,
        "running" => JobState::Running,
        "completed" => JobState::Completed,
        "failed" => JobState::Failed,
        _ => return Err(OpFailed(OperationError::Statement)),
    };
    let attempts: i16 = row.try_get("attempts")?;
    Ok(RowState {
        id,
        generation,
        state,
        attempts,
    })
}

fn job_id(row: &sqlx::postgres::PgRow) -> Result<JobId, OpFailed> {
    let text: String = row.try_get("id")?;
    JobId::parse(&text).ok_or(OpFailed(OperationError::Statement))
}

async fn engine_permit(shared: &Shared) -> Result<SemaphorePermit<'_>, ()> {
    shared.permit.acquire().await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    use super::local_deadline;
    use crate::engine::{CANCEL_MARGIN, CLAIM_TTL};

    #[tokio::test(start_paused = true)]
    async fn local_deadline_is_28s_after_sent() {
        let sent = Instant::now();
        assert_eq!(
            CLAIM_TTL.checked_sub(CANCEL_MARGIN),
            Some(Duration::from_secs(28))
        );
        assert_eq!(local_deadline(sent), sent + Duration::from_secs(28));
    }
}
