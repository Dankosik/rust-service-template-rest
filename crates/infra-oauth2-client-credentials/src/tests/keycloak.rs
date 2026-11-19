use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderValue, Method, Request, StatusCode, Uri, header};
use infra_outbound_http::Client;
use jsonwebtoken::jwk::Jwk;
use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_RSA_SHA256};
use secrecy::SecretString;
use tokio::{sync::watch, task::JoinHandle, time::Instant};
use url::Url;
use uuid::Uuid;

use super::{
    AcquisitionError, Algorithm, EXCHANGE_CAPACITY, Options, Rejection, TOKEN_LIMITS,
    acquisition_deadline, exchange_target, nonzero, service_target, token_context,
};
use crate::{
    IssuerUrl, PrivateKeyJwt, Registration, ServiceTokenTarget, ServiceTokens, TokenUrl,
    token::Token,
};

const KEY_ID: &str = "key-1";
const CATALOG: &str = "catalog";
const CATALOG_SCOPE: &str = "catalog-audience";
const BILLING: &str = "billing";
const BILLING_SCOPE: &str = "billing-audience";
const UPSTREAM: &str = "upstream";
const UPSTREAM_SECRET: &str = "upstream-fixture-secret";

struct Realm {
    origin: Url,
    http: Client,
    admin: String,
    name: String,
    driver_shutdown: watch::Sender<()>,
    drivers: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(serde::Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    azp: String,
    iat: u64,
    exp: u64,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    aud: serde_json::Value,
}

impl Realm {
    async fn create() -> Self {
        let origin = std::env::var("OAUTH_TEST_KEYCLOAK_URL").unwrap_or_else(|_| {
            panic!("OAUTH_TEST_KEYCLOAK_URL is unset; run just test-integration-oauth")
        });
        let origin = Url::parse(&origin).unwrap();
        let http = Client::try_new_for_test_http(origin.as_str(), TOKEN_LIMITS).unwrap();
        let (driver_shutdown, _) = watch::channel(());
        let mut realm = Self {
            origin,
            http,
            admin: String::new(),
            name: format!("oauth-{}", Uuid::new_v4()),
            driver_shutdown,
            drivers: Mutex::default(),
        };
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
        for (recipient, scope) in [(CATALOG, CATALOG_SCOPE), (BILLING, BILLING_SCOPE)] {
            realm
                .client(&serde_json::json!({
                    "clientId": recipient,
                    "protocol": "openid-connect",
                    "publicClient": false,
                    "standardFlowEnabled": false,
                    "secret": format!("{recipient}-fixture-secret"),
                }))
                .await;
            realm
                .admin_post(
                    &format!("/admin/realms/{}/client-scopes", realm.name),
                    &serde_json::json!({
                        "name": scope,
                        "protocol": "openid-connect",
                        "protocolMappers": [audience_mapper(recipient)],
                    }),
                )
                .await;
        }
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
            .execute(
                request,
                &operation_context::OperationContext::with_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap()
    }

    async fn token(&self, realm: &str, fields: &[(&str, &str)]) -> String {
        let token = self.form_post(self.token_endpoint(realm), fields).await;
        token["access_token"].as_str().unwrap().to_owned()
    }

    async fn accepts_token(&self, recipient: &str, token: &Token) -> bool {
        let endpoint = self
            .origin
            .join(&format!(
                "/realms/{}/protocol/openid-connect/token/introspect",
                self.name
            ))
            .unwrap();
        let secret = format!("{recipient}-fixture-secret");
        let result = self
            .form_post(
                endpoint,
                &[
                    ("client_id", recipient),
                    ("client_secret", &secret),
                    ("token", access_token(token)),
                ],
            )
            .await;
        result["active"].as_bool().unwrap()
    }

    async fn form_post(&self, endpoint: Url, fields: &[(&str, &str)]) -> serde_json::Value {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let response = self
            .send(
                Request::post(endpoint.as_str())
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Bytes::from(body))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "POST {}",
            endpoint.path()
        );
        serde_json::from_slice(response.body()).unwrap_or_else(|_| panic!("invalid form response"))
    }

    async fn admin_request(
        &self,
        method: &Method,
        path: &str,
        body: Bytes,
        expected_status: StatusCode,
    ) -> http::Response<Bytes> {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", self.admin)).unwrap();
        authorization.set_sensitive(true);
        let request = Request::builder()
            .method(method.clone())
            .uri(self.origin.join(path).unwrap().as_str())
            .header(header::AUTHORIZATION, authorization)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap();
        let response = self.send(request).await;
        assert_eq!(response.status(), expected_status, "{method} {path}");
        response
    }

    async fn admin_post(&self, path: &str, body: &serde_json::Value) -> http::Response<Bytes> {
        self.admin_request(
            &Method::POST,
            path,
            Bytes::from(serde_json::to_vec(body).unwrap()),
            StatusCode::CREATED,
        )
        .await
    }

    async fn client(&self, representation: &serde_json::Value) -> http::Response<Bytes> {
        self.admin_post(
            &format!("/admin/realms/{}/clients", self.name),
            representation,
        )
        .await
    }

    async fn caller(
        &self,
        algorithm: Algorithm,
        key: &KeyPair,
        access_token_lifespan: Option<u64>,
    ) -> (Registration, String) {
        let client_id = caller(algorithm);
        let mut client = serde_json::json!({
            "clientId": client_id,
            "protocol": "openid-connect",
            "publicClient": false,
            "serviceAccountsEnabled": true,
            "standardFlowEnabled": false,
            "fullScopeAllowed": false,
            "clientAuthenticatorType": "client-jwt",
            "attributes": {
                "use.jwks.string": "true",
                "jwks.string": serde_json::json!({"keys": [public_jwk(algorithm, key, KEY_ID)]}).to_string(),
                "standard.token.exchange.enabled": "true",
            },
            "defaultClientScopes": ["basic"],
            "optionalClientScopes": [CATALOG_SCOPE, BILLING_SCOPE],
        });
        if let Some(lifespan) = access_token_lifespan {
            client["attributes"]["access.token.lifespan"] = lifespan.to_string().into();
        }
        let response = self.client(&client).await;
        let location = response.headers()[header::LOCATION].to_str().unwrap();
        let location = self.origin.join(location).unwrap();
        let id = Uuid::parse_str(location.path_segments().unwrap().next_back().unwrap()).unwrap();
        let path = format!("/admin/realms/{}/clients/{id}", self.name);
        (self.registration(algorithm, key, KEY_ID).await, path)
    }

    async fn replace_client_jwks(&self, path: &str, keys: &[Jwk]) -> Instant {
        let response = self
            .admin_request(&Method::GET, path, Bytes::new(), StatusCode::OK)
            .await;
        let mut client: serde_json::Value = serde_json::from_slice(response.body())
            .unwrap_or_else(|_| panic!("invalid client representation"));
        let jwks = &mut client["attributes"]["jwks.string"];
        assert!(jwks.is_string(), "registered client is missing its JWK Set");
        *jwks = serde_json::json!({"keys": keys}).to_string().into();
        let body = Bytes::from(serde_json::to_vec(&client).unwrap());

        let update_started = Instant::now();
        self.admin_request(&Method::PUT, path, body, StatusCode::NO_CONTENT)
            .await;
        update_started
    }

    async fn registration(
        &self,
        algorithm: Algorithm,
        key: &KeyPair,
        key_id: &str,
    ) -> Registration {
        let endpoint = self.token_endpoint(&self.name);
        let issuer = IssuerUrl::parse_for_test_http(&self.issuer()).unwrap();
        let options = Options {
            token_url: TokenUrl::parse_for_test_http(endpoint.as_str()).unwrap(),
            client_id: caller(algorithm),
            auth: PrivateKeyJwt {
                private_key: SecretString::from(key.serialize_pem()),
                key_id: key_id.to_owned(),
                algorithm,
                audience: issuer.clone(),
            },
            provider_concurrency: nonzero(32),
        };
        Registration::prepare_with_oidc_discovery_for_test_http(issuer, options)
            .await
            .unwrap()
    }

    fn service_tokens(
        &self,
        registration: &Registration,
        target: ServiceTokenTarget,
        recipient: &str,
    ) -> ServiceTokens {
        let (tokens, driver) = registration.service_tokens(target, resource_origin(recipient));
        let mut shutdown = self.driver_shutdown.subscribe();
        self.drivers
            .lock()
            .unwrap()
            .push(tokio::spawn(driver.run(async move {
                let _ = shutdown.changed().await;
            })));
        tokens
    }

    async fn fresh_catalog_token(
        &self,
        registration: &Registration,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.service_tokens(registration, service_target(&[CATALOG_SCOPE]), CATALOG)
            .service_token(acquisition_deadline(&token_context()))
            .await
    }

    async fn observe_catalog_expiry(
        &self,
        token: &Token,
        issued_lifespan: u64,
        mut last_active_started: Instant,
    ) -> (Instant, Instant) {
        let deadline = token.requested_at + Duration::from_secs(issued_lifespan + 5);
        let first_inactive_received = tokio::time::timeout_at(deadline, async {
            loop {
                let probe_started = Instant::now();
                if !self.accepts_token(CATALOG, token).await {
                    break Instant::now();
                }
                last_active_started = probe_started;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("the original bearer token must become inactive near its expiration");
        let earliest_expiry = token.requested_at + Duration::from_secs(issued_lifespan - 1);
        assert!(
            first_inactive_received >= earliest_expiry,
            "the original bearer token became inactive before its expiration"
        );
        assert!(
            first_inactive_received.duration_since(last_active_started) <= Duration::from_secs(1),
            "the final active probe and first inactive reply must bracket expiry within one second"
        );
        (last_active_started, first_inactive_received)
    }

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

    async fn finish(self) {
        self.driver_shutdown.send_replace(());
        let drivers = self.drivers.into_inner().unwrap();
        for driver in drivers {
            tokio::time::timeout(Duration::from_secs(2), driver)
                .await
                .unwrap()
                .unwrap();
        }
    }
}

impl Algorithm {
    const ALL: [Self; 3] = [Self::Rs256, Self::Ps256, Self::Es256];
}

fn caller(algorithm: Algorithm) -> String {
    format!("caller-{algorithm:?}").to_lowercase()
}

fn resource_origin(recipient: &str) -> Uri {
    format!("https://{recipient}.example").parse().unwrap()
}

fn key_pair(algorithm: Algorithm) -> KeyPair {
    KeyPair::generate_for(match algorithm {
        Algorithm::Rs256 | Algorithm::Ps256 => &PKCS_RSA_SHA256,
        Algorithm::Es256 => &PKCS_ECDSA_P256_SHA256,
    })
    .unwrap()
}

fn public_jwk(algorithm: Algorithm, key: &KeyPair, key_id: &str) -> Jwk {
    let pem = key.serialize_pem();
    let encoding = match algorithm {
        Algorithm::Rs256 | Algorithm::Ps256 => {
            jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
        }
        Algorithm::Es256 => jsonwebtoken::EncodingKey::from_ec_pem(pem.as_bytes()),
    }
    .unwrap();
    let mut jwk = Jwk::from_encoding_key(&encoding, algorithm.into()).unwrap();
    jwk.common.key_id = Some(key_id.to_owned());
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

fn access_token(token: &Token) -> &str {
    token
        .header
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
}

fn claims(token: &Token) -> Claims {
    jsonwebtoken::dangerous::insecure_decode::<Claims>(access_token(token))
        .unwrap()
        .claims
}

fn audiences(claims: &Claims) -> Vec<&str> {
    match &claims.aud {
        serde_json::Value::String(audience) => vec![audience.as_str()],
        serde_json::Value::Array(audiences) => audiences
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn one_registration_issues_recipient_specific_service_tokens_and_exchanges_a_subject() {
    let realm = Realm::create().await;
    for algorithm in Algorithm::ALL {
        let key = key_pair(algorithm);
        let (registration, _) = realm.caller(algorithm, &key, None).await;
        let catalog =
            realm.service_tokens(&registration, service_target(&[CATALOG_SCOPE]), CATALOG);
        let billing =
            realm.service_tokens(&registration, service_target(&[BILLING_SCOPE]), BILLING);

        for (tokens, intended, other) in
            [(&catalog, CATALOG, BILLING), (&billing, BILLING, CATALOG)]
        {
            let service = tokens
                .service_token(acquisition_deadline(&token_context()))
                .await
                .unwrap();
            let claims = claims(&service);
            assert_eq!(claims.iss, realm.issuer());
            assert_eq!(claims.azp, caller(algorithm));
            assert_eq!(audiences(&claims), [intended]);
            assert!(realm.accepts_token(intended, &service).await);
            assert!(!realm.accepts_token(other, &service).await);
        }
        catalog.clear_cached();
        catalog
            .service_token(acquisition_deadline(&token_context()))
            .await
            .unwrap();

        let subject = realm.subject_token().await;
        let subject_claims: Claims = jsonwebtoken::dangerous::insecure_decode(
            secrecy::ExposeSecret::expose_secret(&subject),
        )
        .unwrap()
        .claims;
        let exchanged = registration
            .exchange_tokens(
                exchange_target(&[CATALOG_SCOPE, BILLING_SCOPE], Some(CATALOG)),
                resource_origin(CATALOG),
                EXCHANGE_CAPACITY,
            )
            .unwrap()
            .exchange(&subject, acquisition_deadline(&token_context()))
            .await
            .unwrap();
        let exchanged = claims(&exchanged);
        assert_eq!(exchanged.sub, subject_claims.sub);
        assert_eq!(exchanged.azp, caller(algorithm));
        assert_eq!(audiences(&exchanged), [CATALOG]);
    }
    realm.finish().await;
}

#[tokio::test]
#[expect(
    clippy::print_stdout,
    reason = "the integration drill reports non-secret key retirement and token expiry observations"
)]
async fn retiring_a_client_key_preserves_issued_access_until_expiry() {
    const REPLACEMENT_KEY_ID: &str = "key-2";
    const LIFESPAN_SECONDS: u64 = 20;

    let realm = Realm::create().await;
    let algorithm = Algorithm::Es256;
    let old_key = key_pair(algorithm);
    let new_key = key_pair(algorithm);
    let old_jwk = public_jwk(algorithm, &old_key, KEY_ID);
    let new_jwk = public_jwk(algorithm, &new_key, REPLACEMENT_KEY_ID);
    assert!(
        old_jwk.algorithm != new_jwk.algorithm,
        "key material must differ"
    );

    let (old_registration, client_path) = realm
        .caller(algorithm, &old_key, Some(LIFESPAN_SECONDS))
        .await;
    let new_registration = realm
        .registration(algorithm, &new_key, REPLACEMENT_KEY_ID)
        .await;
    let issued_with_old_key = realm.fresh_catalog_token(&old_registration).await.unwrap();
    let issued_claims = claims(&issued_with_old_key);
    assert_eq!(issued_claims.iss, realm.issuer());
    assert_eq!(issued_claims.azp, caller(algorithm));
    assert_eq!(audiences(&issued_claims), [CATALOG]);
    let mut issued_scopes = issued_claims.scope.split_ascii_whitespace();
    assert!(issued_scopes.any(|scope| scope == CATALOG_SCOPE));
    let issued_lifespan = issued_claims.exp.checked_sub(issued_claims.iat).unwrap();
    assert_eq!(issued_lifespan, LIFESPAN_SECONDS);
    assert!(realm.accepts_token(CATALOG, &issued_with_old_key).await);
    assert!(!realm.accepts_token(BILLING, &issued_with_old_key).await);

    realm
        .replace_client_jwks(&client_path, &[old_jwk, new_jwk.clone()])
        .await;
    for registration in [&old_registration, &new_registration] {
        let token = realm.fresh_catalog_token(registration).await.unwrap();
        assert!(
            realm.accepts_token(CATALOG, &token).await,
            "both keys must work during overlap"
        );
    }

    let withdrawal_started = realm.replace_client_jwks(&client_path, &[new_jwk]).await;
    let withdrawal_ack_ms = withdrawal_started.elapsed().as_nanos().div_ceil(1_000_000);
    assert_eq!(
        realm
            .fresh_catalog_token(&old_registration)
            .await
            .unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidClient)
    );
    let old_key_rejected_ms = withdrawal_started.elapsed().as_nanos().div_ceil(1_000_000);
    let first_post_withdrawal_probe_started = Instant::now();
    assert!(
        realm.accepts_token(CATALOG, &issued_with_old_key).await,
        "the original bearer token must stay active after the old key stops issuing tokens"
    );
    let post_withdrawal_active_ms = first_post_withdrawal_probe_started
        .duration_since(withdrawal_started)
        .as_millis();
    let new_token = realm.fresh_catalog_token(&new_registration).await.unwrap();
    assert!(realm.accepts_token(CATALOG, &new_token).await);
    let (last_active_started, first_inactive_received) = realm
        .observe_catalog_expiry(
            &issued_with_old_key,
            issued_lifespan,
            first_post_withdrawal_probe_started,
        )
        .await;

    let live_control = realm.fresh_catalog_token(&new_registration).await.unwrap();
    assert!(realm.accepts_token(CATALOG, &live_control).await);
    assert!(!realm.accepts_token(BILLING, &live_control).await);
    let control_claims = claims(&live_control);
    assert_eq!(control_claims.iss, issued_claims.iss);
    assert_eq!(control_claims.sub, issued_claims.sub);
    assert_eq!(control_claims.azp, issued_claims.azp);
    assert_eq!(audiences(&control_claims), [CATALOG]);
    assert!(
        control_claims.scope == issued_claims.scope,
        "requested scope must remain stable across client key rotation"
    );

    let last_active_probe_start_ms = last_active_started
        .duration_since(withdrawal_started)
        .as_millis();
    let first_inactive_reply_ms = first_inactive_received
        .duration_since(withdrawal_started)
        .as_nanos()
        .div_ceil(1_000_000);
    println!(
        "keycloak_client_key_retirement caller={} recipient={CATALOG} scope={CATALOG_SCOPE} \
         overlap=both_keys_accepted old_key_after_withdrawal=invalid_client new_key_after_withdrawal=accepted \
         iat={} exp={} ttl_seconds={issued_lifespan} withdrawal_ack_ms={withdrawal_ack_ms} \
         old_key_rejected_ms={old_key_rejected_ms} post_withdrawal_active_ms={post_withdrawal_active_ms} \
         last_active_probe_start_ms={last_active_probe_start_ms} first_inactive_reply_ms={first_inactive_reply_ms} \
         later_new_token_active=true",
        issued_claims.azp, issued_claims.iat, issued_claims.exp,
    );

    realm.finish().await;
}

#[tokio::test]
async fn refusals_carry_the_error_code_keycloak_reports() {
    let realm = Realm::create().await;
    let algorithm = Algorithm::Es256;
    let key = key_pair(algorithm);
    let (registered, _) = realm.caller(algorithm, &key, None).await;

    let stranger = realm
        .registration(algorithm, &key_pair(algorithm), KEY_ID)
        .await;
    assert_eq!(
        realm
            .service_tokens(&stranger, ServiceTokenTarget::default(), CATALOG)
            .service_token(acquisition_deadline(&token_context()))
            .await
            .unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidClient)
    );

    assert_eq!(
        realm
            .service_tokens(&registered, service_target(&["not-a-scope"]), CATALOG)
            .service_token(acquisition_deadline(&token_context()))
            .await
            .unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidScope)
    );

    let subject = SecretString::from("not-a-token");
    assert_eq!(
        registered
            .exchange_tokens(
                exchange_target(&[CATALOG_SCOPE], Some(CATALOG)),
                resource_origin(CATALOG),
                EXCHANGE_CAPACITY,
            )
            .unwrap()
            .exchange(&subject, acquisition_deadline(&token_context()))
            .await
            .unwrap_err(),
        AcquisitionError::Rejected(Rejection::InvalidRequest)
    );
    realm.finish().await;
}
