use std::{future::poll_fn, sync::Arc, task::Poll};

use bytes::Bytes;
use compact_str::CompactString;
use foldhash::HashSet;
use http::HeaderValue;
use infra_jobs::{EnqueueError, EnqueueOptions, Enqueued, JobId, json_payload_len};
use infra_postgres::{Tx, TxMode};

use super::{Delivery, delivery_version};
use crate::protocol::MAX_BODY_BYTES;

const PREFLIGHT_BYTES_PER_POLL: usize = 4096;
const JSON_STRING_QUOTES_BYTES: usize = 2;

pub const JSON_CONTENT_TYPE: &str = "application/json";

#[derive(Clone, Debug)]
pub struct Producer {
    endpoint_ids: Arc<HashSet<String>>,
}

impl Producer {
    #[must_use]
    pub fn new(endpoint_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            endpoint_ids: Arc::new(endpoint_ids.into_iter().collect()),
        }
    }

    pub async fn enqueue(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
        endpoint_id: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<JobId, OutboundError> {
        self.preflight(endpoint_id, body.len(), content_type)
            .await?;
        let delivery = Delivery {
            version: delivery_version(),
            endpoint_id: CompactString::from(endpoint_id),
            content_type: CompactString::from(content_type),
            body: Bytes::from(body),
        };
        match infra_jobs::enqueue(tx, &delivery, EnqueueOptions::default()).await? {
            Enqueued::Created(id) => Ok(id),
            Enqueued::Duplicate => Err(OutboundError::UnexpectedDuplicate),
        }
    }

    async fn preflight(
        &self,
        endpoint_id: &str,
        body_len: usize,
        content_type: &str,
    ) -> Result<(), OutboundError> {
        if body_len > MAX_BODY_BYTES {
            return Err(OutboundError::BodyTooLarge);
        }
        let encoded_body_len =
            base64::encoded_len(body_len, true).ok_or(OutboundError::BodyTooLarge)?;
        if !self.endpoint_ids.contains(endpoint_id) {
            return Err(OutboundError::UnknownEndpoint);
        }

        let empty_len = Delivery::empty_serialized_len().map_err(EnqueueError::Serialize)?;
        let mut budget = PREFLIGHT_BYTES_PER_POLL;
        let content_type_len = escaped_len(content_type, &mut budget, |fragment| {
            HeaderValue::from_bytes(fragment.as_bytes())
                .map(drop)
                .map_err(|_| OutboundError::InvalidContentType)
        })
        .await?;
        let endpoint_id_len = escaped_len(endpoint_id, &mut budget, |_| Ok(())).await?;
        let serialized_len = empty_len + encoded_body_len + content_type_len + endpoint_id_len;
        if serialized_len > infra_jobs::MAX_PAYLOAD_BYTES {
            return Err(EnqueueError::PayloadTooLarge {
                bytes: serialized_len,
            }
            .into());
        }
        Ok(())
    }
}

async fn escaped_len(
    field: &str,
    budget: &mut usize,
    mut admit: impl FnMut(&str) -> Result<(), OutboundError>,
) -> Result<usize, OutboundError> {
    let mut len = 0;
    let mut rest = field;
    while !rest.is_empty() {
        let end = rest.floor_char_boundary(*budget);
        if end == 0 {
            yield_once().await;
            *budget = PREFLIGHT_BYTES_PER_POLL;
            continue;
        }
        let (fragment, tail) = rest.split_at(end);
        admit(fragment)?;
        len +=
            json_payload_len(fragment).map_err(EnqueueError::Serialize)? - JSON_STRING_QUOTES_BYTES;
        *budget -= end;
        rest = tail;
    }
    Ok(len)
}

async fn yield_once() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}

#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("outbound webhook endpoint is not configured")]
    UnknownEndpoint,
    #[error("outbound webhook body exceeds the maximum size")]
    BodyTooLarge,
    #[error("outbound webhook content type is invalid")]
    InvalidContentType,
    #[error("outbound webhook enqueue failed")]
    Enqueue(#[from] EnqueueError),
    #[error("outbound webhook enqueue unexpectedly deduplicated")]
    UnexpectedDuplicate,
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use http::HeaderValue;

    #[tokio::test]
    async fn preflight_matches_delivery_json_at_the_jobs_limit() {
        let endpoint_id = "partner\\\"é";
        let outbound = super::Producer::new([endpoint_id.to_owned()]);
        for body_bytes in [0, 1, 2, 3, 4, super::MAX_BODY_BYTES] {
            for extra in [0, 1] {
                let mut delivery = super::Delivery {
                    version: super::delivery_version(),
                    endpoint_id: endpoint_id.into(),
                    content_type: format!("{}é\t\\\"", "x".repeat(4095)).into(),
                    body: bytes::Bytes::from(vec![0xff; body_bytes]),
                };
                assert!(HeaderValue::from_str(&delivery.content_type).is_ok());
                let initial_bytes = serde_json::to_vec(&delivery).unwrap().len();
                let expected_bytes = infra_jobs::MAX_PAYLOAD_BYTES + extra;
                delivery
                    .content_type
                    .push_str(&"x".repeat(expected_bytes - initial_bytes));
                assert_eq!(serde_json::to_vec(&delivery).unwrap().len(), expected_bytes);

                let result = outbound
                    .preflight(endpoint_id, body_bytes, &delivery.content_type)
                    .await;
                if extra == 0 {
                    assert!(result.is_ok(), "exact jobs limit must be admitted");
                } else {
                    assert_matches!(
                        result,
                        Err(super::OutboundError::Enqueue(
                            infra_jobs::EnqueueError::PayloadTooLarge { bytes }
                        )) if bytes == expected_bytes);
                }
            }
        }
    }

    #[tokio::test]
    async fn preflight_preserves_validation_priority_including_an_invalid_oversized_tail() {
        let outbound = super::Producer::new(["partner".to_owned()]);
        let content_type = format!("{}\n", "x".repeat(infra_jobs::MAX_PAYLOAD_BYTES));
        assert_matches!(
            outbound
                .preflight("missing", super::MAX_BODY_BYTES + 1, &content_type)
                .await,
            Err(super::OutboundError::BodyTooLarge)
        );
        assert_matches!(
            outbound.preflight("missing", 0, &content_type).await,
            Err(super::OutboundError::UnknownEndpoint)
        );
        assert_matches!(
            outbound.preflight("partner", 0, &content_type).await,
            Err(super::OutboundError::InvalidContentType)
        );
    }

    #[test]
    fn preflight_yields_and_self_wakes_before_inspecting_the_next_fragment() {
        use std::{
            future::Future as _,
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
            task::{Context, Poll, Wake, Waker},
        };

        #[derive(Default)]
        struct Wakes(AtomicUsize);
        impl Wake for Wakes {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let notifications = Arc::new(Wakes::default());
        let waker = Waker::from(Arc::clone(&notifications));
        let mut context = Context::from_waker(&waker);
        let outbound = super::Producer::new(["partner".to_owned()]);
        let content_type = format!("{}\n", "x".repeat(8192));
        let mut preflight = std::pin::pin!(outbound.preflight("partner", 0, &content_type));
        let verdict = (0..64)
            .find_map(|_| {
                let before = notifications.0.load(Ordering::Relaxed);
                let poll = preflight.as_mut().poll(&mut context);
                assert!(poll.is_ready() || notifications.0.load(Ordering::Relaxed) > before);
                match poll {
                    Poll::Pending => None,
                    Poll::Ready(verdict) => Some(verdict),
                }
            })
            .expect("preflight reaches a verdict");
        assert!(notifications.0.load(Ordering::Relaxed) >= 1, "8 KiB yields");
        assert_matches!(verdict, Err(super::OutboundError::InvalidContentType));
    }
}
