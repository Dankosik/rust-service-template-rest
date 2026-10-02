//! The adapter against a real authorization server: a throwaway Keycloak
//! container. Run through `make test-integration-oauth`, which starts it
//! (`scripts/ci/test-integration-oauth.sh`) and sets `OAUTH_TEST_KEYCLOAK_URL`.
//!
//! This proves what the local fixture cannot: that a real server accepts the
//! client assertion for every supported algorithm, issues a service token and
//! an exchanged token for these request forms, and reports refusals with the
//! error codes the adapter classifies.

use std::time::Duration;

use bytes::Bytes;
use http::{Method, Request, StatusCode, header};
use infra_outbound_http::Client;
use jsonwebtoken::jwk::Jwk;
use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_RSA_SHA256};
use secrecy::SecretString;
use tokio::time::Instant;
use url::Url;
use uuid::Uuid;

use super::{AcquisitionError, Algorithm, Credentials, Options, Rejection, TOKEN_LIMITS};
use crate::subject_key;

const KEY_ID: &str = "key-1";
/// The audience client an exchanged token is addressed to.
const TARGET: &str = "target";
/// The client whose service token stands in for a verified inbound token.
const UPSTREAM: &str = "upstream";
const UPSTREAM_SECRET: &str = "upstream-fixture-secret";

/// One throwaway realm of the Keycloak this run was pointed at.
struct Realm {
    origin: Url,
    http: Client,
    admin: String,
    name: String,
}

#[derive(serde::Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    azp: String,
    #[serde(default)]
    aud: serde_json::Value,
}

impl Realm {
    async fn create() -> Self {
        let origin = std::env::var("OAUTH_TEST_KEYCLOAK_URL").unwrap_or_else(|_| {
            panic!("OAUTH_TEST_KEYCLOAK_URL is unset; run scripts/ci/test-integration-oauth.sh")
        });
        let origin = Url::parse(&origin).unwrap();
        let http = Client::new_for_test_http(&origin, TOKEN_LIMITS).unwrap();
        let mut realm = Self {
            origin,
            http,
            admin: String::new(),
            name: format!("oauth-{}", Uuid::new_v4()),
        };
        // The bootstrap administrator of the Compose service.
        let admin = realm
            .token(
                "master",
                &[
                    ("grant_type", "password"),
                    ("client_id", "admin-cli"),
                    ("username", "admin"),
                    ("password", "admin"),
                ],
            )
            .await;
        realm.admin = admin;
        realm
            .admin_post(
                "/admin/realms",
                &serde_json::json!({"realm": realm.name, "enabled": true}),
            )
            .await;
        realm
            .client(&serde_json::json!({"clientId": TARGET, "publicClient": false}))
            .await;
        // Its tokens name `caller-*` clients in `aud`, as token exchange
        // requires of a subject token.
        realm
            .client(&serde_json::json!({
                "clientId": UPSTREAM,
                "publicClient": false,
                "serviceAccountsEnabled": true,
                "standardFlowEnabled": false,
                "secret": UPSTREAM_SECRET,
                "protocolMappers": Algorithm::ALL
                    .iter()
                    .map(|algorithm| audience_mapper(&caller(*algorithm)))
                    .collect::<Vec<_>>(),
            }))
            .await;
        realm
    }

    fn issuer(&self) -> String {
        format!(
            "{}/realms/{}",
            self.origin.as_str().trim_end_matches('/'),
            self.name
        )
    }

    fn token_endpoint(&self, realm: &str) -> Url {
        self.origin
            .join(&format!("/realms/{realm}/protocol/openid-connect/token"))
            .unwrap()
    }

    async fn send(&self, request: Request<Bytes>) -> http::Response<Bytes> {
        self.http
            .execute(request, Instant::now() + Duration::from_secs(30))
            .await
            .unwrap()
    }

    /// A token from a plain form request, used only to prepare the realm.
    async fn token(&self, realm: &str, fields: &[(&str, &str)]) -> String {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let response = self
            .send(
                Request::post(self.token_endpoint(realm).as_str())
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Bytes::from(body))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{:?}", response.body());
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        body["access_token"].as_str().unwrap().to_owned()
    }

    async fn admin_post(&self, path: &str, body: &serde_json::Value) {
        let response = self
            .send(
                Request::builder()
                    .method(Method::POST)
                    .uri(self.origin.join(path).unwrap().as_str())
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.admin))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Bytes::from(serde_json::to_vec(body).unwrap()))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "{path}: {:?}",
            response.body()
        );
    }

    async fn client(&self, representation: &serde_json::Value) {
        self.admin_post(
            &format!("/admin/realms/{}/clients", self.name),
            representation,
        )
        .await;
    }

    /// Registers the calling service for `algorithm` with the public half of
    /// `key`, as an operator would, and returns its credentials.
    async fn caller(
        &self,
        algorithm: Algorithm,
        key: &KeyPair,
        scopes: &[&str],
        audience: Option<&str>,
    ) -> Credentials {
        let client_id = caller(algorithm);
        self.client(&serde_json::json!({
            "clientId": client_id,
            "publicClient": false,
            "serviceAccountsEnabled": true,
            "standardFlowEnabled": false,
            "clientAuthenticatorType": "client-jwt",
            "attributes": {
                "use.jwks.string": "true",
                "jwks.string": serde_json::json!({"keys": [public_jwk(algorithm, key)]}).to_string(),
                "standard.token.exchange.enabled": "true",
            },
            "protocolMappers": [audience_mapper(TARGET)],
        }))
        .await;
        self.credentials(algorithm, key, scopes, audience)
    }

    fn credentials(
        &self,
        algorithm: Algorithm,
        key: &KeyPair,
        scopes: &[&str],
        audience: Option<&str>,
    ) -> Credentials {
        let endpoint = self.token_endpoint(&self.name);
        let options = Options {
            token_url: endpoint.to_string(),
            client_id: caller(algorithm),
            private_key: SecretString::from(key.serialize_pem()),
            key_id: KEY_ID.to_owned(),
            algorithm,
            assertion_audience: self.issuer(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            audience: audience.map(str::to_owned),
            exchange_cache_capacity: 1024,
        };
        Credentials::prepare(
            options,
            &endpoint,
            Client::new_for_test_http(&self.origin, TOKEN_LIMITS).unwrap(),
        )
        .unwrap()
    }

    /// A real access token whose audience includes every `caller-*` client.
    async fn subject_token(&self) -> SecretString {
        SecretString::from(
            self.token(
                &self.name,
                &[
                    ("grant_type", "client_credentials"),
                    ("client_id", UPSTREAM),
                    ("client_secret", UPSTREAM_SECRET),
                ],
            )
            .await,
        )
    }
}

impl Algorithm {
    const ALL: [Self; 3] = [Self::Rs256, Self::Ps256, Self::Es256];
}

fn caller(algorithm: Algorithm) -> String {
    format!("caller-{algorithm:?}").to_lowercase()
}

fn key_pair(algorithm: Algorithm) -> KeyPair {
    KeyPair::generate_for(match algorithm {
        Algorithm::Rs256 | Algorithm::Ps256 => &PKCS_RSA_SHA256,
        Algorithm::Es256 => &PKCS_ECDSA_P256_SHA256,
    })
    .unwrap()
}

fn public_jwk(algorithm: Algorithm, key: &KeyPair) -> Jwk {
    let pem = key.serialize_pem();
    let encoding = match algorithm {
        Algorithm::Rs256 | Algorithm::Ps256 => {
            jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
        }
        Algorithm::Es256 => jsonwebtoken::EncodingKey::from_ec_pem(pem.as_bytes()),
    }
    .unwrap();
    let mut jwk = Jwk::from_encoding_key(&encoding, algorithm.into()).unwrap();
    jwk.common.key_id = Some(KEY_ID.to_owned());
    // Keycloak ignores a key that does not declare itself a signing key.
    jwk.common.public_key_use = Some(jsonwebtoken::jwk::PublicKeyUse::Signature);
    jwk
}

fn audience_mapper(client_id: &str) -> serde_json::Value {
    serde_json::json!({
        "name": format!("audience-{client_id}"),
        "protocol": "openid-connect",
        "protocolMapper": "oidc-audience-mapper",
        "config": {
            "included.client.audience": client_id,
            "access.token.claim": "true",
        },
    })
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

/// The claims of an access token Keycloak returned over this direct
/// connection; the resource server, not this client, verifies signatures.
fn claims(token: &super::super::Token) -> Claims {
    let bearer = token.header.to_str().unwrap();
    jsonwebtoken::dangerous::insecure_decode::<Claims>(bearer.strip_prefix("Bearer ").unwrap())
        .unwrap()
        .claims
}

fn audiences(claims: &Claims) -> Vec<&str> {
    match &claims.aud {
        serde_json::Value::String(audience) => vec![audience.as_str()],
        serde_json::Value::Array(audiences) => audiences
            .iter()
            .filter_map(|value| value.as_str())
            .collect(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn every_algorithm_authenticates_the_client_and_exchanges_a_subject_token() {
    let realm = Realm::create().await;
    for algorithm in Algorithm::ALL {
        let key = key_pair(algorithm);
        let credentials = realm.caller(algorithm, &key, &[], Some(TARGET)).await;

        let service = credentials.service_token(deadline()).await.unwrap();
        let service = claims(&service);
        assert_eq!(service.iss, realm.issuer());
        assert_eq!(service.azp, caller(algorithm));
        // A second request signs a new assertion; a reused `jti` is refused.
        *credentials.cached() = super::Cached::default();
        credentials.service_token(deadline()).await.unwrap();

        let subject = realm.subject_token().await;
        let subject_claims: Claims = jsonwebtoken::dangerous::insecure_decode(
            secrecy::ExposeSecret::expose_secret(&subject),
        )
        .unwrap()
        .claims;
        let key = subject_key(secrecy::ExposeSecret::expose_secret(&subject).as_bytes());
        let exchanged = credentials
            .exchange(key, &subject, deadline())
            .await
            .unwrap();
        let exchanged = claims(&exchanged);
        // The subject stays the upstream caller; this service is the party
        // the token was issued to, and the integration its audience.
        assert_eq!(exchanged.sub, subject_claims.sub);
        assert_eq!(exchanged.azp, caller(algorithm));
        assert_eq!(audiences(&exchanged), [TARGET]);
    }
}

#[tokio::test]
async fn refusals_carry_the_error_code_keycloak_reports() {
    let realm = Realm::create().await;
    let algorithm = Algorithm::Es256;
    let key = key_pair(algorithm);
    let registered = realm.caller(algorithm, &key, &[], Some(TARGET)).await;

    // An assertion signed by a key the server does not hold for this client.
    let stranger = realm.credentials(algorithm, &key_pair(algorithm), &[], None);
    assert_eq!(
        stranger.service_token(deadline()).await.unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidClient)
    );

    let unknown_scope = realm.credentials(algorithm, &key, &["not-a-scope"], None);
    assert_eq!(
        unknown_scope.service_token(deadline()).await.unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidScope)
    );

    // RFC 8693 section 2.2.2 reports an unusable subject token as
    // `invalid_request`, so it is not distinguishable from a malformed form.
    let subject = SecretString::from("not-a-token");
    let key = subject_key(b"not-a-token");
    assert_eq!(
        registered
            .exchange(key, &subject, deadline())
            .await
            .unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidRequest)
    );
}
