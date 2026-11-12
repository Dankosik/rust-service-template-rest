use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use http::HeaderValue;
use tokio::time::Instant;

use crate::AcquisitionError;

/// A token stops being reused this long before it expires, as in Go's `oauth2`.
const REUSE_MARGIN: Duration = Duration::from_secs(10);
/// A resource rejection evicts only a token at least this old. The provider would
/// answer a younger one with the same token, so a resource that refuses every
/// token costs one token request per this period, not one per call.
pub(crate) const EVICTION_MIN_AGE: Duration = Duration::from_secs(30);

/// A sensitive `Bearer` header value and the instant it stops being reused.
#[derive(derive_more::Debug)]
#[debug("Token([REDACTED])")]
pub(crate) struct Token {
    pub(crate) header: HeaderValue,
    pub(crate) requested_at: Instant,
    /// `None` when the provider gave no lifetime: use only for its requesting call.
    reuse_until: Option<Instant>,
    rejected: AtomicBool,
}

impl Token {
    /// Builds the sensitive header for `access_token` and its reuse cutoff
    /// from the lifetime `expires_in` the provider gave, counted from the
    /// request start `requested_at`.
    pub(crate) fn bearer(
        access_token: &str,
        requested_at: Instant,
        expires_in: Option<u64>,
    ) -> Result<Self, AcquisitionError> {
        let value = format!("Bearer {access_token}");
        // The parser treats leading spaces as scheme separators. Match the
        // original token so provider whitespace is rejected rather than trimmed.
        if shared_rules::rfc6750::token([value.as_bytes()]) != Ok(access_token) {
            return Err(AcquisitionError::InvalidResponse);
        }
        let mut header =
            HeaderValue::try_from(value).map_err(|_| AcquisitionError::InvalidResponse)?;
        header.set_sensitive(true);
        let reuse_until = match expires_in {
            None => None,
            Some(lifetime) => {
                let expiry = requested_at
                    .checked_add(Duration::from_secs(lifetime))
                    .ok_or(AcquisitionError::InvalidResponse)?;
                if Instant::now() >= expiry {
                    return Err(AcquisitionError::InvalidResponse);
                }
                Some(expiry.checked_sub(REUSE_MARGIN).unwrap_or(requested_at))
            }
        };
        Ok(Self {
            header,
            requested_at,
            reuse_until,
            rejected: AtomicBool::new(false),
        })
    }

    pub(crate) fn is_reusable(&self, now: Instant) -> bool {
        self.reusable_until(now).is_some()
    }

    pub(crate) fn reusable_until(&self, now: Instant) -> Option<Instant> {
        self.reuse_until
            .filter(|&until| now < until && !self.is_rejected())
    }

    pub(crate) fn is_rejected(&self) -> bool {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn reject(&self) {
        if Instant::now().saturating_duration_since(self.requested_at) >= EVICTION_MIN_AGE {
            // Reuse reads this bit alone; no other state is published with it.
            self.rejected.store(true, Ordering::Relaxed);
        }
    }
}
