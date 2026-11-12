use std::time::{Duration, SystemTime, UNIX_EPOCH};

use secrecy::ExposeSecret as _;
use uuid::Uuid;

use crate::{AcquisitionError, Algorithm, ConfigurationError, Options};

/// RFC 7523 section 2.2 and the OIDF client-assertion notice: one string
/// audience, a fresh `jti`, and an assertion signed for at most this long.
const ASSERTION_LIFETIME_SECS: u64 = 60;
/// An assertion is dated this far back, as Go's `oauth2/jws` does, so a
/// provider whose clock is slightly behind does not see it as issued in the
/// future or expiring too late.
const ASSERTION_BACKDATE_SECS: u64 = 10;
const ASSERTION_TYP: &str = "client-authentication+jwt";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// Builds and signs client assertions. Construction proves the key matches
/// its algorithm; every subsequent token request signs a fresh one.
pub(crate) struct Signer {
    key: jsonwebtoken::EncodingKey,
    header: jsonwebtoken::Header,
    client_id: String,
    assertion_audience: String,
}

impl Signer {
    pub(crate) fn try_new(options: &Options, now: SystemTime) -> Result<Self, ConfigurationError> {
        let invalid_key = ConfigurationError {
            key: "private_key",
            reason: "must be a PEM key matching its configured algorithm",
        };
        let pem = options.private_key.expose_secret().as_bytes();
        let key = match options.algorithm {
            Algorithm::Rs256 | Algorithm::Ps256 => jsonwebtoken::EncodingKey::from_rsa_pem(pem),
            Algorithm::Es256 => jsonwebtoken::EncodingKey::from_ec_pem(pem),
        }
        .map_err(|_| invalid_key)?;
        let mut header = jsonwebtoken::Header::new(options.algorithm.into());
        header.kid = Some(options.key_id.clone());
        header.typ = Some(ASSERTION_TYP.to_owned());
        let signer = Self {
            key,
            header,
            client_id: options.client_id.clone(),
            assertion_audience: options.assertion_audience.clone(),
        };
        let elapsed = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ConfigurationError {
                key: "system_clock",
                reason: "must be at or after the Unix epoch",
            })?;
        signer.sign_at(elapsed).map_err(|_| invalid_key)?;
        Ok(signer)
    }

    pub(crate) fn form_fields<'a>(&'a self, assertion: &'a str) -> [(&'static str, &'a str); 3] {
        [
            ("client_id", self.client_id.as_str()),
            ("client_assertion_type", CLIENT_ASSERTION_TYPE),
            ("client_assertion", assertion),
        ]
    }

    /// Signs one fresh assertion. Never cached: RFC 7523bis requires a
    /// single-use `jti` and every shortlisted server enforces it.
    pub(crate) fn sign(&self) -> Result<String, AcquisitionError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AcquisitionError::Assertion)?;
        self.sign_at(elapsed)
            .map_err(|_| AcquisitionError::Assertion)
    }

    fn sign_at(&self, elapsed: Duration) -> jsonwebtoken::errors::Result<String> {
        let issued = elapsed.as_secs().saturating_sub(ASSERTION_BACKDATE_SECS);
        let claims = AssertionClaims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.assertion_audience,
            iat: issued,
            nbf: issued,
            exp: issued + ASSERTION_LIFETIME_SECS,
            jti: Uuid::new_v4().to_string(),
        };
        jsonwebtoken::encode(&self.header, &claims, &self.key)
    }
}

#[derive(serde::Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    nbf: u64,
    exp: u64,
    jti: String,
}
