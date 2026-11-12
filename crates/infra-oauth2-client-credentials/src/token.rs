use std::time::Duration;

use http::HeaderValue;
use tokio::time::Instant;

use crate::AcquisitionError;

/// A token stops being reused this long before it expires, as in Go's `oauth2`.
const REUSE_MARGIN: Duration = Duration::from_secs(10);
/// A resource 401 evicts only a token at least this old. The provider would
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
    pub(crate) reuse_until: Option<Instant>,
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
        let mut header = HeaderValue::try_from(format!("Bearer {access_token}"))
            .map_err(|_| AcquisitionError::InvalidResponse)?;
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
        })
    }

    pub(crate) fn is_reusable(&self, now: Instant) -> bool {
        self.reusable_until(now).is_some()
    }

    pub(crate) fn reusable_until(&self, now: Instant) -> Option<Instant> {
        self.reuse_until.filter(|&until| now < until)
    }

    pub(crate) fn is_evictable(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.requested_at) >= EVICTION_MIN_AGE
    }
}
