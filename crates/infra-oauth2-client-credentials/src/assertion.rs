use std::time::{Duration, SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret as _, SecretString};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{AcquisitionError, Algorithm, ConfigurationError};

/// RFC 7523 §2.2
const ASSERTION_LIFETIME_SECS: u64 = 60;
const ASSERTION_BACKDATE_SECS: u64 = 10;
const ASSERTION_TYP: &str = "client-authentication+jwt";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

pub(crate) struct SignerConfig {
    pub(crate) client_id: String,
    pub(crate) private_key: SecretString,
    pub(crate) key_id: String,
    pub(crate) algorithm: Algorithm,
    pub(crate) audience: String,
}

pub(crate) struct Signer {
    key: jsonwebtoken::EncodingKey,
    header: jsonwebtoken::Header,
    client_id: String,
    assertion_audience: String,
}

impl Signer {
    pub(crate) fn try_new(
        config: SignerConfig,
        now: SystemTime,
    ) -> Result<Self, ConfigurationError> {
        let invalid_key = ConfigurationError {
            key: "private_key",
            reason: "must be a PEM key matching its configured algorithm",
        };
        let pem = config.private_key.expose_secret().as_bytes();
        let key = match config.algorithm {
            Algorithm::Rs256 | Algorithm::Ps256 => jsonwebtoken::EncodingKey::from_rsa_pem(pem),
            Algorithm::Es256 => jsonwebtoken::EncodingKey::from_ec_pem(pem),
        }
        .map_err(|_| invalid_key)?;
        let mut header = jsonwebtoken::Header::new(config.algorithm.into());
        header.kid = Some(config.key_id);
        header.typ = Some(ASSERTION_TYP.to_owned());
        let signer = Self {
            key,
            header,
            client_id: config.client_id,
            assertion_audience: config.audience,
        };
        let since_unix_epoch = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ConfigurationError {
                key: "system_clock",
                reason: "must be at or after the Unix epoch",
            })?;
        signer.sign_at(since_unix_epoch).map_err(|_| invalid_key)?;
        Ok(signer)
    }

    pub(crate) fn form_fields<'a>(&'a self, assertion: &'a str) -> [(&'static str, &'a str); 3] {
        [
            ("client_id", self.client_id.as_str()),
            ("client_assertion_type", CLIENT_ASSERTION_TYPE),
            ("client_assertion", assertion),
        ]
    }

    /// RFC 10070 §4
    pub(crate) fn sign(&self) -> Result<Zeroizing<String>, AcquisitionError> {
        let since_unix_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AcquisitionError::Assertion)?;
        self.sign_at(since_unix_epoch)
            .map_err(|_| AcquisitionError::Assertion)
    }

    fn sign_at(
        &self,
        since_unix_epoch: Duration,
    ) -> jsonwebtoken::errors::Result<Zeroizing<String>> {
        let issued = since_unix_epoch
            .as_secs()
            .saturating_sub(ASSERTION_BACKDATE_SECS);
        let claims = AssertionClaims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.assertion_audience,
            iat: issued,
            nbf: issued,
            exp: issued + ASSERTION_LIFETIME_SECS,
            jti: Uuid::new_v4(),
        };
        jsonwebtoken::encode(&self.header, &claims, &self.key).map(Zeroizing::new)
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
    jti: Uuid,
}
