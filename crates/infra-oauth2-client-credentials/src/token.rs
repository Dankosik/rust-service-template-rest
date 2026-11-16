use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use http::HeaderValue;
use tokio::time::Instant;

use crate::AcquisitionError;

const REUSE_MARGIN: Duration = Duration::from_secs(10);
pub(crate) const EVICTION_MIN_AGE: Duration = Duration::from_secs(30);

#[derive(derive_more::Debug)]
#[debug("Token([REDACTED])")]
pub(crate) struct Token {
    pub(crate) header: HeaderValue,
    pub(crate) requested_at: Instant,
    expires_at: Option<Instant>,
    rejected: AtomicBool,
}

impl Token {
    pub(crate) fn bearer(
        access_token: &str,
        requested_at: Instant,
        expires_in: Option<u64>,
    ) -> Result<Self, AcquisitionError> {
        let value = zeroize::Zeroizing::new(format!("Bearer {access_token}"));
        if shared_rules::rfc6750::token([value.as_bytes()]) != Ok(access_token) {
            return Err(AcquisitionError::InvalidResponse);
        }
        let mut header =
            HeaderValue::from_str(value.as_str()).map_err(|_| AcquisitionError::InvalidResponse)?;
        header.set_sensitive(true);
        let expires_at = match expires_in {
            None => None,
            Some(lifetime) => {
                let expiry = requested_at
                    .checked_add(Duration::from_secs(lifetime))
                    .ok_or(AcquisitionError::InvalidResponse)?;
                if Instant::now() >= expiry {
                    return Err(AcquisitionError::InvalidResponse);
                }
                Some(expiry)
            }
        };
        Ok(Self {
            header,
            requested_at,
            expires_at,
            rejected: AtomicBool::new(false),
        })
    }

    pub(crate) fn is_reusable(&self, now: Instant) -> bool {
        self.reusable_until(now).is_some()
    }

    pub(crate) fn reusable_until(&self, now: Instant) -> Option<Instant> {
        self.expires_at
            .and_then(|expiry| expiry.checked_sub(REUSE_MARGIN))
            .filter(|&until| now < until && !self.is_rejected())
    }

    pub(crate) fn check_expiry(&self, now: Instant) -> Result<(), AcquisitionError> {
        if self.expires_at.is_some_and(|expiry| now >= expiry) {
            return Err(AcquisitionError::InvalidResponse);
        }
        Ok(())
    }

    pub(crate) fn is_rejected(&self) -> bool {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn reject_if_old_enough(&self) {
        if Instant::now().saturating_duration_since(self.requested_at) >= EVICTION_MIN_AGE {
            self.rejected.store(true, Ordering::Relaxed);
        }
    }
}
