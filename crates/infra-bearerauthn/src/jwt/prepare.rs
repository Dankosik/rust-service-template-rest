use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{JwtVerifier, TokenProfile};
use crate::{
    EndpointUrl, IssuerUrl, JsonObject, JwtAlgorithm, PreparationError, PreparationPhase,
    PreparationReason, ProviderFailure, Verifier,
    claims::ClaimPolicy,
    jwks::{JwksSource, KEY_REJECTIONS_METRIC, KeySetFailure, allowed_algorithms},
    provider::{Document, ProviderClient},
    refresh::{KeyStore, run_refresh_worker},
};

const STARTUP_BUDGET: Duration = Duration::from_secs(6);
const MAX_ECHOED_ISSUER_BYTES: usize = 256;

#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("JwtOptions([REDACTED])")]
pub struct JwtOptions {
    pub issuer: IssuerUrl,
    pub audiences: Vec<String>,
    pub token_profile: TokenProfile,
    pub algorithms: Vec<JwtAlgorithm>,
    pub jwks_uri: Option<EndpointUrl>,
    pub jwks_max_age: Duration,
}

pub type RefreshTask = BoxFuture<'static, ()>;

pub async fn prepare_jwt(
    options: JwtOptions,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), PreparationError> {
    let startup_deadline = Instant::now() + STARTUP_BUDGET;
    let provider = ProviderClient::try_new();
    let provider = enforce_startup_deadline(startup_deadline, PreparationPhase::Client, provider)?;
    prepare_with_provider(options, provider, cancel, startup_deadline).await
}

async fn prepare_with_provider(
    options: JwtOptions,
    provider: ProviderClient,
    cancel: CancellationToken,
    startup_deadline: Instant,
) -> Result<(Verifier, RefreshTask), PreparationError> {
    metrics::describe_counter!(
        KEY_REJECTIONS_METRIC,
        "Rejected JWKS entries by closed reason"
    );
    let algorithms = allowed_algorithms(&options.algorithms);
    let claim_policy = if options.algorithms.is_empty()
        || options.jwks_max_age < shared_rules::oidc_jwt::MIN_JWKS_MAX_AGE
    {
        Err(PreparationError::invalid_options())
    } else {
        ClaimPolicy::try_new(options.issuer.as_str(), options.audiences)
    };
    let claim_policy =
        enforce_startup_deadline(startup_deadline, PreparationPhase::Options, claim_policy)?;
    let source = match options.jwks_uri {
        Some(uri) => JwksSource::Configured(uri),
        None => JwksSource::Discovery(options.issuer),
    };
    let key_set = source
        .fetch(&provider, algorithms, startup_deadline)
        .await
        .and_then(|keys| {
            if keys.is_empty() {
                Err(KeySetFailure::NoUsableKeys)
            } else {
                Ok(keys)
            }
        })
        .map_err(PreparationError::from);
    let phase = key_set
        .as_ref()
        .err()
        .map_or(PreparationPhase::Jwks, PreparationError::phase);
    let key_set = enforce_startup_deadline(startup_deadline, phase, key_set)?;
    let key_store = KeyStore::new(key_set);
    let verifier = Verifier::jwt(JwtVerifier::new(
        claim_policy,
        options.token_profile,
        algorithms,
        key_store.clone(),
        options.jwks_max_age,
    ));
    let refresh_task: RefreshTask = Box::pin(run_refresh_worker(
        key_store,
        provider,
        source,
        algorithms,
        cancel,
        options.jwks_max_age,
    ));
    enforce_startup_deadline(
        startup_deadline,
        PreparationPhase::Jwks,
        Ok((verifier, refresh_task)),
    )
}

fn enforce_startup_deadline<T>(
    deadline: Instant,
    phase: PreparationPhase,
    result: Result<T, PreparationError>,
) -> Result<T, PreparationError> {
    if Instant::now() >= deadline {
        return Err(PreparationError::new(phase, PreparationReason::Timeout));
    }
    result
}

impl PreparationError {
    fn issuer_mismatch(configured: &IssuerUrl, discovered: String) -> Self {
        let discovered = if discovered.len() <= MAX_ECHOED_ISSUER_BYTES
            && IssuerUrl::parse(&discovered).is_ok()
        {
            discovered
        } else {
            "<issuer value omitted>".to_owned()
        };
        Self::IssuerMismatch {
            configured: configured.as_str().to_owned(),
            discovered,
        }
    }
}

pub(crate) async fn discover_jwks_uri(
    provider: &ProviderClient,
    issuer: &IssuerUrl,
    deadline: Instant,
) -> Result<EndpointUrl, PreparationError> {
    let discovery = fetch_discovery(provider, issuer, deadline).await?;
    if discovery.issuer != issuer.as_str() {
        return Err(PreparationError::issuer_mismatch(issuer, discovery.issuer));
    }
    EndpointUrl::parse(&discovery.jwks_uri).map_err(|_| {
        PreparationError::new(PreparationPhase::Discovery, PreparationReason::InvalidUrl)
    })
}

async fn fetch_discovery(
    provider: &ProviderClient,
    issuer: &IssuerUrl,
    deadline: Instant,
) -> Result<Discovery, PreparationError> {
    let path = issuer.url().path().trim_suffix('/');
    let mut openid = issuer.url().clone();
    openid.set_path(&format!("{path}/.well-known/openid-configuration"));
    let discovery = match fetch_metadata(provider, &openid, deadline).await {
        Err(
            first @ (PreparationReason::Fetch(ProviderFailure::Status(_))
            | PreparationReason::Parse),
        ) => {
            let mut rfc8414 = issuer.url().clone();
            rfc8414.set_path(&format!("/.well-known/oauth-authorization-server{path}"));
            match fetch_metadata(provider, &rfc8414, deadline).await {
                // RFC 8414 §3
                Err(PreparationReason::Fetch(ProviderFailure::Status(404 | 410))) => Err(first),
                result => result,
            }
        }
        result => result,
    };
    discovery.map_err(|reason| PreparationError::new(PreparationPhase::Discovery, reason))
}

async fn fetch_metadata(
    provider: &ProviderClient,
    url: &url::Url,
    deadline: Instant,
) -> Result<Discovery, PreparationReason> {
    let bytes = provider
        .get_document(url, Document::Discovery, Some(deadline))
        .await
        .map_err(PreparationReason::Fetch)?;
    serde_json::from_slice::<JsonObject<Discovery>>(&bytes)
        .map(|JsonObject(discovery)| discovery)
        .map_err(|_| PreparationReason::Parse)
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use bytes::Bytes;
    use http::{Request, Response, StatusCode};
    use http_body_util::{BodyExt as _, Full};
    use hyper::{body::Incoming, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use jsonwebtoken::Algorithm;
    use tokio::sync::watch;

    use super::{Document, JwtAlgorithm, TokenProfile};
    use crate::{
        jwks::tests::{ensure_crypto_provider, jwk, rsa_signing},
        test_helpers::runtime,
        test_metrics::{Diagnostics, labeled},
    };

    #[test]
    fn preparation_errors_are_closed_and_an_issuer_mismatch_names_both_issuers() {
        use crate::{IssuerUrl, PreparationError, PreparationPhase, PreparationReason};
        assert_eq!(
            PreparationError::new(
                PreparationPhase::Jwks,
                PreparationReason::Fetch(crate::ProviderFailure::Status(503))
            )
            .to_string(),
            "authentication preparation failed during Jwks: Fetch(Status(503))"
        );
        let configured = IssuerUrl::parse("https://issuer.example").unwrap();
        let error =
            PreparationError::issuer_mismatch(&configured, "https://issuer.example/".to_owned());
        assert_eq!(error.phase(), PreparationPhase::Discovery);
        assert_eq!(error.reason(), PreparationReason::IssuerMismatch);
        assert_eq!(
            error.to_string(),
            "authentication preparation failed during Discovery: IssuerMismatch \
             (configured issuer \"https://issuer.example\", \
             discovered issuer \"https://issuer.example/\")"
        );
        for discovered in [
            "https://issuer.example/?token=private".to_owned(),
            format!("https://issuer.example/{}", "a".repeat(256)),
            "not an issuer\nprivate".to_owned(),
        ] {
            let rendered = PreparationError::issuer_mismatch(&configured, discovered).to_string();
            assert!(rendered.contains("<issuer value omitted>"), "{rendered}");
            assert!(!rendered.contains("private") && !rendered.contains("aaaa"));
        }
    }

    const FIXTURE_HOST: &str = "authn.fixture.test";

    type MetadataRoute = (&'static str, StatusCode, String);

    async fn serve_metadata_connection(
        stream: tokio::net::TcpStream,
        acceptor: tokio_rustls::TlsAcceptor,
        port: String,
        routes: Arc<[MetadataRoute]>,
        requested: watch::Sender<Vec<String>>,
    ) {
        let Ok(stream) = acceptor.accept(stream).await else {
            return;
        };
        let service = service_fn(move |request: Request<Incoming>| {
            let port = port.clone();
            let routes = Arc::clone(&routes);
            let requested = requested.clone();
            async move {
                let target = request
                    .uri()
                    .path_and_query()
                    .expect("origin-form request target")
                    .as_str()
                    .to_owned();
                let body = request.into_body().collect().await?.to_bytes();
                assert!(body.is_empty());
                let mut route = None;
                requested.send_modify(|paths| {
                    let seen = paths.iter().filter(|path| path.as_str() == target).count();
                    route = routes
                        .iter()
                        .filter(|(path, ..)| *path == target)
                        .take(seen + 1)
                        .last();
                    paths.push(target);
                });
                let (status, body) = route.map_or(
                    (StatusCode::NOT_FOUND, Bytes::new()),
                    |(_, status, body)| (*status, Bytes::from(body.replace("PORT", &port))),
                );
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(status)
                        .body(Full::new(body))
                        .unwrap(),
                )
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    }

    async fn metadata_server(
        routes: Vec<MetadataRoute>,
    ) -> (std::net::SocketAddr, Vec<u8>, watch::Receiver<Vec<String>>) {
        let (acceptor, root) = crate::provider::fixture_acceptor(FIXTURE_HOST);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let port = address.port().to_string();
        let routes: Arc<[MetadataRoute]> = Arc::from(routes);
        let (requested, requests) = watch::channel(Vec::new());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_metadata_connection(
                    stream,
                    acceptor.clone(),
                    port.clone(),
                    Arc::clone(&routes),
                    requested.clone(),
                ));
            }
        });
        (address, root, requests)
    }

    async fn prepare(
        address: std::net::SocketAddr,
        root: &[u8],
        jwks_uri: Option<&str>,
    ) -> Result<(crate::Verifier, super::RefreshTask), crate::PreparationError> {
        let startup_deadline = tokio::time::Instant::now() + super::STARTUP_BUDGET;
        let origin = format!("https://{FIXTURE_HOST}:{}", address.port());
        let options = super::JwtOptions {
            issuer: crate::IssuerUrl::parse(&format!("{origin}/tenant")).unwrap(),
            audiences: vec!["api".to_owned()],
            token_profile: TokenProfile::ResourceServer,
            algorithms: vec![JwtAlgorithm::Rs256, JwtAlgorithm::Es256],
            jwks_max_age: Duration::from_hours(1),
            jwks_uri: jwks_uri
                .map(|path| crate::EndpointUrl::parse(&format!("{origin}{path}")).unwrap()),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let provider =
            crate::provider::new_fixture_client(FIXTURE_HOST, address, root, cancel.clone())
                .unwrap();
        super::prepare_with_provider(options, provider, cancel, startup_deadline).await
    }

    #[tokio::test(start_paused = true)]
    async fn an_expired_startup_budget_takes_precedence_over_a_stage_failure() {
        use crate::{PreparationError, PreparationPhase, PreparationReason};
        use tokio::time::{Instant, advance};

        let deadline = Instant::now() + Duration::from_secs(6);
        let stage = PreparationPhase::Jwks;
        let parse_failure = PreparationError::new(stage, PreparationReason::Parse);
        let complete = |result| super::enforce_startup_deadline(deadline, stage, result);
        assert_eq!(
            complete(Err(parse_failure.clone())),
            Err(parse_failure.clone())
        );

        advance(Duration::from_secs(6)).await;
        let timeout = PreparationError::new(stage, PreparationReason::Timeout);
        assert_eq!(complete(Err(parse_failure)), Err(timeout.clone()));
        assert_eq!(complete(Ok(())), Err(timeout));
    }

    #[tokio::test]
    async fn unusable_oidc_discovery_falls_back_to_rfc8414_and_explicit_jwks_skips_it() {
        ensure_crypto_provider();
        let key = jwk(&rsa_signing(), Algorithm::RS256, Some("fixture"), true);
        let metadata = format!(
            r#"{{"issuer":"https://{FIXTURE_HOST}:PORT/tenant","jwks_uri":"https://{FIXTURE_HOST}:PORT/keys"}}"#
        );
        let (address, root, requested) = metadata_server(vec![
            (
                "/tenant/.well-known/openid-configuration",
                StatusCode::OK,
                "<html>Default page</html>".to_owned(),
            ),
            (
                "/.well-known/oauth-authorization-server/tenant",
                StatusCode::OK,
                metadata,
            ),
            (
                "/keys",
                StatusCode::OK,
                serde_json::json!({ "keys": [key] }).to_string(),
            ),
        ])
        .await;
        let _prepared = prepare(address, &root, None).await.unwrap();
        assert_eq!(
            *requested.borrow(),
            [
                "/tenant/.well-known/openid-configuration",
                "/.well-known/oauth-authorization-server/tenant",
                "/keys"
            ]
        );
        let before_explicit = requested.borrow().len();
        let _prepared = prepare(address, &root, Some("/keys")).await.unwrap();
        assert_eq!(&requested.borrow()[before_explicit..], ["/keys"]);
    }

    #[tokio::test]
    async fn a_provider_without_metadata_fails_preparation_with_its_status() {
        use crate::{PreparationPhase, PreparationReason, ProviderFailure};
        let (address, root, requested) = metadata_server(Vec::new()).await;
        let error = prepare(address, &root, None).await.err().unwrap();
        assert_eq!(error.phase(), PreparationPhase::Discovery);
        assert_eq!(
            error.reason(),
            PreparationReason::Fetch(ProviderFailure::Status(404))
        );
        assert_eq!(requested.borrow().len(), 2);
    }

    #[tokio::test]
    async fn an_explicitly_empty_jwks_cannot_start_a_verifier() {
        use crate::{PreparationPhase, PreparationReason};

        let (address, root, _) =
            metadata_server(vec![("/keys", StatusCode::OK, r#"{"keys":[]}"#.to_owned())]).await;
        let error = prepare(address, &root, Some("/keys")).await.err().unwrap();
        assert_eq!(error.phase(), PreparationPhase::Jwks);
        assert_eq!(error.reason(), PreparationReason::NoUsableKeys);
    }

    #[tokio::test]
    async fn periodic_discovery_moves_trust_while_the_previous_endpoint_still_serves_keys() {
        use crate::{Failure, jwks::tests::ec_signing, refresh::REFRESH_INTERVAL};
        use jsonwebtoken::{Algorithm::ES256, Header, encode};

        ensure_crypto_provider();
        let old = ec_signing();
        let new = ec_signing();
        let document =
            |key| serde_json::json!({"keys": [jwk(key, ES256, Some("key"), true)]}).to_string();
        let metadata = |issuer_path, path| {
            serde_json::json!({
                "issuer": format!("https://{FIXTURE_HOST}:PORT/{issuer_path}"),
                "jwks_uri": format!("https://{FIXTURE_HOST}:PORT/{path}"),
            })
            .to_string()
        };
        let discovery = "/tenant/.well-known/openid-configuration";
        let (address, root, mut requests) = metadata_server(vec![
            (discovery, StatusCode::OK, metadata("tenant", "a")),
            (discovery, StatusCode::OK, metadata("tenant", "b")),
            (discovery, StatusCode::OK, metadata("other", "untrusted")),
            (discovery, StatusCode::OK, metadata("tenant", "b")),
            ("/a", StatusCode::OK, document(&old)),
            ("/b", StatusCode::OK, document(&new)),
            ("/b", StatusCode::SERVICE_UNAVAILABLE, String::new()),
            ("/untrusted", StatusCode::OK, document(&old)),
        ])
        .await;
        let (verifier, mut worker) = prepare(address, &root, None).await.unwrap();
        let signed = |key| {
            let mut header = Header::new(ES256);
            header.kid = Some("key".to_owned());
            let claims = serde_json::json!({
                "iss": format!("https://{FIXTURE_HOST}:{}/tenant", address.port()),
                "aud": "api", "sub": "subject", "exp": u64::MAX
            });
            format!("Bearer {}", encode(&header, &claims, key).unwrap())
        };
        let old = signed(&old);
        let new = signed(&new);
        let old = crate::parse_bearer([old.as_bytes()]).unwrap();
        let new = crate::parse_bearer([new.as_bytes()]).unwrap();
        verifier.verify(&old).await.unwrap();

        tokio::time::pause();
        assert!(futures_util::poll!(worker.as_mut()).is_pending());
        tokio::time::advance(REFRESH_INTERVAL).await;
        assert!(futures_util::poll!(worker.as_mut()).is_pending());
        tokio::time::resume();
        let task = tokio::spawn(worker);
        tokio::time::timeout(
            Duration::from_secs(5),
            requests.wait_for(|paths| paths.iter().any(|path| path == "/b")),
        )
        .await
        .expect("periodic discovery requests B without a token miss")
        .unwrap();
        verifier
            .verify(&new)
            .await
            .expect("the scheduled refresh installs B");
        assert_eq!(verifier.verify(&old).await.unwrap_err(), Failure::Invalid);
        for failure in ["issuer mismatch", "unavailable key endpoint"] {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            tokio::time::resume();
            assert_eq!(
                verifier.verify(&old).await.unwrap_err(),
                Failure::Invalid,
                "{failure}"
            );
            verifier.verify(&new).await.unwrap();
        }
        assert_eq!(
            *requests.borrow(),
            [discovery, "/a", discovery, "/b", discovery, discovery, "/b"]
        );
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(3600 - 50)).await;
        tokio::time::resume();
        assert_eq!(
            verifier.verify(&new).await.unwrap_err(),
            Failure::Unavailable
        );
        task.abort();
    }

    #[tokio::test]
    async fn withdrawing_all_signing_keys_revokes_cached_and_unseen_tokens_until_keys_return() {
        use crate::Failure;
        use crate::jwks::tests::ec_signing;
        use jsonwebtoken::{EncodingKey, Header, encode};

        ensure_crypto_provider();
        let signing = ec_signing();
        let replacement_signing = ec_signing();
        let mut encryption = jwk(&ec_signing(), Algorithm::ES256, Some("encryption"), false);
        encryption["use"] = "enc".into();
        let key_document = |kid, signer: &EncodingKey| {
            let mut key = jwk(signer, Algorithm::ES256, Some(kid), true);
            key["use"] = "sig".into();
            serde_json::json!({ "keys": [key, &encryption] }).to_string()
        };
        for withdrawn in [
            serde_json::json!({"keys": []}),
            serde_json::json!({"keys": [&encryption]}),
        ] {
            let (address, root, requested) = metadata_server(
                [
                    key_document("old", &signing),
                    r#"{"keys":false}"#.to_owned(),
                    r#"{"keys":[{}]}"#.to_owned(),
                    withdrawn.to_string(),
                    key_document("new", &replacement_signing),
                ]
                .into_iter()
                .map(|body| ("/keys", StatusCode::OK, body))
                .collect(),
            )
            .await;
            let (verifier, worker) = prepare(address, &root, Some("/keys")).await.unwrap();
            let worker = tokio::spawn(worker);
            let authorization = |signer: &EncodingKey, kid: &str, subject: &str| {
                let mut header = Header::new(Algorithm::ES256);
                header.kid = Some(kid.to_owned());
                let claims = serde_json::json!({
                    "iss": format!("https://{FIXTURE_HOST}:{}/tenant", address.port()),
                    "aud": "api", "sub": subject,
                    "exp": jsonwebtoken::get_current_timestamp() + 600,
                });
                format!("Bearer {}", encode(&header, &claims, signer).unwrap())
            };
            let cached = authorization(&signing, "old", "cached");
            let unseen = authorization(&signing, "old", "unseen");
            let replacement = authorization(&replacement_signing, "new", "replacement");
            let cached = crate::parse_bearer([cached.as_bytes()]).unwrap();
            let unseen = crate::parse_bearer([unseen.as_bytes()]).unwrap();
            let replacement = crate::parse_bearer([replacement.as_bytes()]).unwrap();
            verifier.verify(&cached).await.unwrap();

            let refresh = || async {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(31)).await;
                tokio::time::resume();
                verifier.verify(&replacement).await
            };
            for response in ["malformed JWKS", "nonempty JWKS without usable keys"] {
                assert_eq!(refresh().await.unwrap_err(), Failure::Invalid, "{response}");
                let retained = authorization(&signing, "old", response);
                let retained = crate::parse_bearer([retained.as_bytes()]).unwrap();
                assert_eq!(
                    verifier.verify(&retained).await.unwrap().subject(),
                    Some(response)
                );
            }

            assert_eq!(refresh().await.unwrap_err(), Failure::Invalid);
            for token in [&cached, &unseen] {
                assert_eq!(verifier.verify(token).await.unwrap_err(), Failure::Invalid);
            }
            assert_eq!(refresh().await.unwrap().subject(), Some("replacement"));
            assert_eq!(
                verifier.verify(&cached).await.unwrap_err(),
                Failure::Invalid
            );
            assert_eq!(*requested.borrow(), ["/keys"; 5]);
            worker.abort();
        }
    }

    #[tokio::test]
    async fn losing_the_refresh_future_releases_current_and_future_unknown_key_waiters() {
        use crate::Failure;
        use jsonwebtoken::{Header, encode};

        ensure_crypto_provider();
        let signing = rsa_signing();
        let key = jwk(&signing, Algorithm::RS256, Some("old"), true);
        let (address, root, _) = metadata_server(vec![(
            "/keys",
            StatusCode::OK,
            serde_json::json!({ "keys": [key] }).to_string(),
        )])
        .await;
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("new".to_owned());
        let claims = serde_json::json!({
            "iss": format!("https://{FIXTURE_HOST}:{}/tenant", address.port()),
            "aud": "api", "sub": "subject", "exp": u64::MAX,
        });
        let jwt = encode(&header, &claims, &signing).unwrap();
        let authorization = format!("Bearer {jwt}");
        let token = crate::parse_bearer([authorization.as_bytes()]).unwrap();
        for abort_started_worker in [false, true] {
            let (verifier, mut worker) = prepare(address, &root, Some("/keys")).await.unwrap();
            tokio::time::pause();
            if abort_started_worker {
                assert!(futures_util::poll!(worker.as_mut()).is_pending());
            }
            tokio::time::advance(Duration::from_secs(31)).await;
            let (waiting, registered) = tokio::sync::oneshot::channel();
            let waiter = tokio::spawn({
                let verifier = verifier.clone();
                let authorization = authorization.clone();
                async move {
                    let token = crate::parse_bearer([authorization.as_bytes()]).unwrap();
                    let mut verification = std::pin::pin!(verifier.verify(&token));
                    assert!(futures_util::poll!(verification.as_mut()).is_pending());
                    waiting.send(()).unwrap();
                    verification.await
                }
            });
            registered.await.unwrap();
            if abort_started_worker {
                let task = tokio::spawn(worker);
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                drop(worker);
            }
            let result = tokio::time::timeout(Duration::from_millis(1), waiter)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.unwrap_err(), Failure::Unavailable);
            assert!(matches!(
                futures_util::poll!(Box::pin(verifier.verify(&token))),
                std::task::Poll::Ready(Err(Failure::Unavailable))
            ));
            tokio::time::resume();
        }
    }

    #[tokio::test]
    async fn a_failing_oidc_location_keeps_its_error_when_rfc8414_is_absent() {
        use crate::{PreparationPhase, PreparationReason, ProviderFailure};
        for ((status, body), fallback, expected) in [
            (
                (StatusCode::SERVICE_UNAVAILABLE, ""),
                StatusCode::NOT_FOUND,
                PreparationReason::Fetch(ProviderFailure::Status(503)),
            ),
            (
                (StatusCode::SERVICE_UNAVAILABLE, ""),
                StatusCode::GONE,
                PreparationReason::Fetch(ProviderFailure::Status(503)),
            ),
            (
                (StatusCode::OK, "{}"),
                StatusCode::NOT_FOUND,
                PreparationReason::Parse,
            ),
        ] {
            let (address, root, requested) = metadata_server(vec![
                (
                    "/tenant/.well-known/openid-configuration",
                    status,
                    body.to_owned(),
                ),
                (
                    "/.well-known/oauth-authorization-server/tenant",
                    fallback,
                    String::new(),
                ),
            ])
            .await;
            let error = prepare(address, &root, None).await.err().unwrap();
            assert_eq!(error.phase(), PreparationPhase::Discovery);
            assert_eq!(error.reason(), expected);
            assert_eq!(
                *requested.borrow(),
                [
                    "/tenant/.well-known/openid-configuration",
                    "/.well-known/oauth-authorization-server/tenant"
                ]
            );
        }
    }

    #[test]
    fn each_provider_exchange_records_one_duration_sample_and_a_bounded_client_span() {
        ensure_crypto_provider();
        let diagnostics = Diagnostics::default();
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = diagnostics.dispatch();
        let key = jwk(&rsa_signing(), Algorithm::RS256, Some("fixture"), true);
        let metadata = format!(
            r#"{{"issuer":"https://{FIXTURE_HOST}:PORT/tenant","jwks_uri":"https://{FIXTURE_HOST}:PORT/keys?tenant=private"}}"#
        );
        let runtime = runtime(false);
        let port = metrics::with_local_recorder(diagnostics.recorder(), || {
            tracing::dispatcher::with_default(&dispatch, || {
                runtime.block_on(async {
                    let (address, root, _) = metadata_server(vec![
                        (
                            "/.well-known/oauth-authorization-server/tenant",
                            StatusCode::OK,
                            metadata,
                        ),
                        (
                            "/keys?tenant=private",
                            StatusCode::OK,
                            serde_json::json!({ "keys": [key] }).to_string(),
                        ),
                    ])
                    .await;
                    let _prepared = prepare(address, &root, None).await.unwrap();
                    address.port()
                })
            })
        });
        let samples = diagnostics.take_histograms();
        let labels = samples
            .iter()
            .map(|key| {
                assert_eq!(key.name(), "authn_provider_request_duration_seconds");
                key.labels()
                    .map(|label| format!("{}={}", label.key(), label.value()))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "operation=discovery,outcome=provider_status_4xx",
                "operation=discovery,outcome=success",
                "operation=jwks,outcome=success",
            ]
        );
        let spans = diagnostics.take_spans();
        let server = format!("server.address=\"{FIXTURE_HOST}\";server.port={port};");
        let client = format!(
            "span=authn_provider;otel.name=\"GET\";otel.kind=\"client\";http.request.method=\"GET\";{server}"
        );
        assert_eq!(
            spans,
            [
                format!(
                    "{client}authn.operation=\"discovery\";http.response.status_code=404;\
                     error.type=\"provider_status_4xx\";otel.status_code=\"ERROR\";"
                ),
                format!("{client}authn.operation=\"discovery\";http.response.status_code=200;"),
                format!("{client}authn.operation=\"jwks\";http.response.status_code=200;"),
            ]
        );
    }

    #[test]
    fn provider_telemetry_distinguishes_startup_deadline_from_abandonment() {
        use std::time::Duration;

        use crate::ProviderFailure;

        let diagnostics = Diagnostics::default();
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = diagnostics.dispatch();
        let runtime = runtime(true);
        metrics::with_local_recorder(diagnostics.recorder(), || {
            tracing::dispatcher::with_default(&dispatch, || {
                runtime.block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let address = listener.local_addr().unwrap();
                    let (_, root) = crate::provider::fixture_acceptor(FIXTURE_HOST);
                    let provider = crate::provider::new_fixture_client(
                        FIXTURE_HOST,
                        address,
                        &root,
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .unwrap();
                    let url = format!("https://{FIXTURE_HOST}:{}/keys", address.port());
                    let url = url::Url::parse(&url).unwrap();
                    let mut pending = Box::pin(provider.get_document(&url, Document::Jwks, None));
                    assert!(futures_util::poll!(pending.as_mut()).is_pending());
                    drop(pending);

                    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
                    assert_eq!(
                        provider
                            .get_document(&url, Document::Jwks, Some(deadline))
                            .await,
                        Err(ProviderFailure::Timeout)
                    );
                });
            });
        });
        let samples = diagnostics.take_histograms();
        let spans = diagnostics.take_spans();
        assert_eq!(samples.len(), 2);
        assert_eq!(spans.len(), 2);
        for (index, outcome) in ["cancelled", "provider_timeout"].into_iter().enumerate() {
            assert!(labeled(&samples[index], &[("outcome", outcome)]));
            assert!(spans[index].ends_with(&format!(
                "error.type=\"{outcome}\";otel.status_code=\"ERROR\";"
            )));
        }
    }
}
