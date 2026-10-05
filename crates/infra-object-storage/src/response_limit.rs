//! Bound bodies the SDK collects before it interprets their status or XML.

use aws_sdk_s3::config::interceptors::BeforeDeserializationInterceptorContextMut;
use aws_sdk_s3::config::{ConfigBag, Intercept, RuntimeComponents};
use aws_sdk_s3::primitives::SdkBody;
use http_body::Frame;
use http_body_util::{BodyExt, Limited};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) enum ResponseLimit {
    /// Client-wide, before GET's generated checksum interceptor.
    Error,
    /// Only nonstreaming operations install this operation-level hook.
    Success,
}

impl Intercept for ResponseLimit {
    fn name(&self) -> &'static str {
        "ObjectStorageResponseLimit"
    }

    fn modify_before_deserialization(
        &self,
        context: &mut BeforeDeserializationInterceptorContextMut<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let response = context.response_mut();
        if response.status().is_success() == matches!(self, Self::Success) {
            let body = std::mem::replace(response.body_mut(), SdkBody::taken());
            *response.body_mut() = bounded(body);
        }
        Ok(())
    }
}

fn bounded(body: SdkBody) -> SdkBody {
    // The SDK collector discards trailers. Discard them before collection so
    // repeated metadata cannot accumulate; empty DATA is not retained either.
    let data = body.map_frame(|frame| Frame::data(frame.into_data().unwrap_or_default()));
    SdkBody::from_body_1_x(Limited::new(data, MAX_RESPONSE_BYTES))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http_body::SizeHint;

    use super::*;

    struct Frames {
        frames: VecDeque<Result<Frame<Bytes>, std::io::Error>>,
        advertised: Option<u64>,
    }

    impl http_body::Body for Frames {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(self.frames.pop_front())
        }

        fn size_hint(&self) -> SizeHint {
            self.advertised
                .map_or_else(SizeHint::new, SizeHint::with_exact)
        }
    }

    #[tokio::test]
    async fn actual_fragmented_data_decides_the_limit_regardless_of_size_hint() {
        for advertised in [None, Some(1), Some(2 * MAX_RESPONSE_BYTES as u64)] {
            for extra in [false, true] {
                let frames = Frames {
                    frames: [
                        Ok(Frame::data(Bytes::from(vec![b'x'; MAX_RESPONSE_BYTES - 1]))),
                        Ok(Frame::data(Bytes::from_static(if extra {
                            b"xx"
                        } else {
                            b"x"
                        }))),
                    ]
                    .into(),
                    advertised,
                };
                let result = bounded(SdkBody::from_body_1_x(frames)).collect().await;
                if extra {
                    assert!(
                        result.unwrap_err().is::<http_body_util::LengthLimitError>(),
                        "advertised={advertised:?}"
                    );
                } else {
                    assert_eq!(result.unwrap().to_bytes(), vec![b'x'; MAX_RESPONSE_BYTES]);
                }
            }
        }
    }

    #[tokio::test]
    async fn collection_discards_metadata_and_propagates_a_late_body_error() {
        let mut frames = VecDeque::new();
        for _ in 0..16 {
            let mut trailers = axum::http::HeaderMap::new();
            trailers.insert("x-provider-metadata", "ignored".parse().unwrap());
            frames.push_back(Ok(Frame::trailers(trailers)));
            frames.push_back(Ok(Frame::data(Bytes::new())));
        }
        frames.push_back(Ok(Frame::data(Bytes::from_static(b"ok"))));
        let collected = bounded(SdkBody::from_body_1_x(Frames {
            frames,
            advertised: None,
        }))
        .collect()
        .await
        .unwrap();
        assert!(collected.trailers().is_none());
        assert_eq!(collected.to_bytes(), "ok");

        let failure = bounded(SdkBody::from_body_1_x(Frames {
            frames: [
                Ok(Frame::data(Bytes::from(vec![b'x'; MAX_RESPONSE_BYTES]))),
                Err(std::io::Error::other("lost EOF")),
            ]
            .into(),
            advertised: Some(MAX_RESPONSE_BYTES as u64),
        }))
        .collect()
        .await
        .unwrap_err();
        assert_eq!(
            failure
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .to_string(),
            "lost EOF"
        );
    }
}
