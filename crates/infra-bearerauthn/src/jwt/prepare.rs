//! Initial trust for the JWT verifier.

use std::{future::Future, pin::Pin, time::Duration};

use serde::Deserialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{JwtVerifier, TokenProfile};
use crate::{
    EndpointUrl, IssuerUrl, JsonObject, JwtAlgorithm, PreparationError, PreparationPhase,
    PreparationReason, ProviderFailure, Verifier,
    claims::ClaimPolicy,
    jwks::{KEY_REJECTIONS_METRIC, fetch_key_set},
    provider::{Document, ProviderClient},
    refresh::{KeyStore, run_refresh_worker},
};

const STARTUP_BUDGET: Duration = Duration::from_secs(6);
/// Bounds provider-supplied text that reaches an operator error.
const MAX_ECHOED_ISSUER_BYTES: usize = 256;

/// Bootstrap input for OIDC discovery and JWT verification.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("JwtOptions([REDACTED])")]
pub struct JwtOptions {
    pub issuer: IssuerUrl,
    /// Audiences of which a token must name at least one; non-empty, with no
    /// empty entry.
    pub audiences: Vec<String>,
    pub token_profile: TokenProfile,
    /// Accepted signature algorithms; non-empty.
    pub algorithms: Vec<JwtAlgorithm>,
    /// The key set location. When set, discovery is skipped; a token's `iss`
    /// is still compared with `issuer` exactly.
    pub jwks_uri: Option<EndpointUrl>,
}

/// The unstarted future that keeps a JWT verifier's installed key set fresh.
/// The caller must spawn it through its process tracker; until then, keys are
/// never refreshed.
pub type RefreshTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Prepares initial trust before traffic admission and returns the one refresh
/// future for bootstrap to spawn through its process tracker. Install the
/// metrics recorder first: the verifier registers its counters here.
/// `cancel` stops only the refresh task. The six-second startup budget starts
/// before client construction. Synchronous work is checked after it returns
/// because it cannot be preempted by the async deadline.
///
/// # Errors
///
/// Returns [`PreparationError`] when options, client preparation, discovery,
/// issuer agreement, or initial key admission fail.
pub async fn prepare_jwt(
    options: JwtOptions,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), PreparationError> {
    let startup_deadline = Instant::now() + STARTUP_BUDGET;
    let provider = ProviderClient::try_new();
    let provider = complete_startup_stage(startup_deadline, PreparationPhase::Client, provider)?;
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
    let claim_policy = if options.algorithms.is_empty() {
        Err(PreparationError::invalid_options())
    } else {
        ClaimPolicy::try_new(options.issuer.as_str().to_owned(), options.audiences)
    };
    let claim_policy =
        complete_startup_stage(startup_deadline, PreparationPhase::Options, claim_policy)?;
    let jwks_uri = if let Some(jwks_uri) = options.jwks_uri {
        jwks_uri
    } else {
        let uri = discover_jwks_uri(&provider, &options.issuer, startup_deadline).await;
        complete_startup_stage(startup_deadline, PreparationPhase::Discovery, uri)?
    };
    let key_set = fetch_key_set(
        &provider,
        &jwks_uri,
        &options.algorithms,
        Some(startup_deadline),
    )
    .await
    .map_err(|failure| PreparationError::new(PreparationPhase::Jwks, failure.into()));
    let key_set = complete_startup_stage(startup_deadline, PreparationPhase::Jwks, key_set)?;
    let key_store = KeyStore::new(key_set);
    let verifier = Verifier::jwt(JwtVerifier::new(
        claim_policy,
        options.token_profile,
        options.algorithms.clone(),
        key_store.clone(),
    ));
    let refresh_task: RefreshTask = Box::pin(run_refresh_worker(
        key_store,
        provider,
        jwks_uri,
        options.algorithms,
        cancel,
    ));
    complete_startup_stage(
        startup_deadline,
        PreparationPhase::Jwks,
        Ok((verifier, refresh_task)),
    )
}

fn complete_startup_stage<T>(
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
    /// Only echo a discovered value that satisfies the issuer grammar and fits
    /// the diagnostic limit.
    fn issuer_mismatch(configured: &IssuerUrl, discovered: &str) -> Self {
        let discovered = if discovered.len() <= MAX_ECHOED_ISSUER_BYTES
            && IssuerUrl::parse(discovered).is_ok()
        {
            discovered.to_owned()
        } else {
            "<issuer value omitted>".to_owned()
        };
        Self::IssuerMismatch {
            configured: configured.as_str().to_owned(),
            discovered,
        }
    }
}

async fn discover_jwks_uri(
    provider: &ProviderClient,
    issuer: &IssuerUrl,
    deadline: Instant,
) -> Result<EndpointUrl, PreparationError> {
    let discovery = fetch_discovery(provider, issuer, deadline).await?;
    if discovery.issuer != issuer.as_str() {
        return Err(PreparationError::issuer_mismatch(issuer, &discovery.issuer));
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
    let error = |reason| PreparationError::new(PreparationPhase::Discovery, reason);
    let path = issuer.url().path();
    let path = path.trim_suffix('/');
    let mut openid = issuer.url().clone();
    openid.set_path(&format!("{path}/.well-known/openid-configuration"));
    let discovery = match fetch_metadata(provider, &openid, deadline).await {
        // A catch-all page can answer the missing OIDC endpoint with 200.
        Err(
            first @ (PreparationReason::Fetch(ProviderFailure::Status(_))
            | PreparationReason::Parse),
        ) => {
            let mut rfc8414 = issuer.url().clone();
            rfc8414.set_path(&format!("/.well-known/oauth-authorization-server{path}"));
            match fetch_metadata(provider, &rfc8414, deadline).await {
                // Keep an OIDC outage visible when RFC 8414 metadata is missing or gone.
                Err(PreparationReason::Fetch(ProviderFailure::Status(404 | 410))) => Err(first),
                result => result,
            }
        }
        result => result,
    };
    discovery.map_err(error)
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

    use jsonwebtoken::Algorithm;

    use super::{Document, JwtAlgorithm, TokenProfile};
    use crate::{
        jwks::tests::{ensure_crypto_provider, jwk, rsa_signing},
        test_metrics::Diagnostics,
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
        let error = PreparationError::issuer_mismatch(&configured, "https://issuer.example/");
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
            let rendered = PreparationError::issuer_mismatch(&configured, &discovered).to_string();
            assert!(rendered.contains("<issuer value omitted>"), "{rendered}");
            assert!(!rendered.contains("private") && !rendered.contains("aaaa"));
        }
    }

    const FIXTURE_HOST: &str = "authn.fixture.test";

    /// Serves `routes` (request target and JSON body, where `PORT` stands for
    /// the listening port) over fixture TLS and answers any other target with
    /// 404. Returns the address, the trusted root and the targets requested.
    async fn metadata_server(
        routes: Vec<(&'static str, String)>,
    ) -> (
        std::net::SocketAddr,
        Vec<u8>,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (acceptor, root) = crate::provider::fixture_acceptor(FIXTURE_HOST);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let port = address.port().to_string();
        let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
        tokio::spawn({
            let requested = Arc::clone(&requested);
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut stream = acceptor.accept(stream).await.unwrap();
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    while !request.array_windows().any(|part| part == b"\r\n\r\n") {
                        let read = stream.read(&mut chunk).await.unwrap();
                        assert_ne!(read, 0);
                        request.extend_from_slice(&chunk[..read]);
                    }
                    let request = String::from_utf8(request).unwrap();
                    let target = request.split(' ').nth(1).unwrap().to_owned();
                    let response = match routes.iter().find(|(route, _)| *route == target) {
                        Some((_, raw)) if raw.starts_with("HTTP/") => raw.clone(),
                        Some((_, body)) => {
                            let body = body.replace("PORT", &port);
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_owned(),
                    };
                    requested.lock().unwrap().push(target);
                    stream.write_all(response.as_bytes()).await.unwrap();
                    let _ = stream.shutdown().await;
                }
            }
        });
        (address, root, requested)
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
            algorithms: vec![JwtAlgorithm::Rs256],
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
        let complete = |result| super::complete_startup_stage(deadline, stage, result);
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
                "<html>Default page</html>".to_owned(),
            ),
            ("/.well-known/oauth-authorization-server/tenant", metadata),
            ("/keys", serde_json::json!({ "keys": [key] }).to_string()),
        ])
        .await;
        let _prepared = prepare(address, &root, None).await.unwrap();
        assert_eq!(
            std::mem::take(&mut *requested.lock().unwrap()),
            [
                "/tenant/.well-known/openid-configuration",
                "/.well-known/oauth-authorization-server/tenant",
                "/keys"
            ]
        );
        let _prepared = prepare(address, &root, Some("/keys")).await.unwrap();
        assert_eq!(*requested.lock().unwrap(), ["/keys"]);
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
        assert_eq!(requested.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_failing_oidc_location_keeps_its_error_when_rfc8414_is_absent() {
        use crate::{PreparationPhase, PreparationReason, ProviderFailure};
        for (response, fallback, expected) in [
            (
                "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                PreparationReason::Fetch(ProviderFailure::Status(503)),
            ),
            (
                "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                "HTTP/1.1 410 Gone\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                PreparationReason::Fetch(ProviderFailure::Status(503)),
            ),
            (
                "{}",
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                PreparationReason::Parse,
            ),
        ] {
            let (address, root, requested) = metadata_server(vec![
                (
                    "/tenant/.well-known/openid-configuration",
                    response.to_owned(),
                ),
                (
                    "/.well-known/oauth-authorization-server/tenant",
                    fallback.to_owned(),
                ),
            ])
            .await;
            let error = prepare(address, &root, None).await.err().unwrap();
            assert_eq!(error.phase(), PreparationPhase::Discovery);
            assert_eq!(error.reason(), expected);
            assert_eq!(
                *requested.lock().unwrap(),
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
        let dispatch = tracing::Dispatch::new(diagnostics.clone());
        let key = jwk(&rsa_signing(), Algorithm::RS256, Some("fixture"), true);
        let metadata = format!(
            r#"{{"issuer":"https://{FIXTURE_HOST}:PORT/tenant","jwks_uri":"https://{FIXTURE_HOST}:PORT/keys?tenant=private"}}"#
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let port = metrics::with_local_recorder(&diagnostics, || {
            tracing::dispatcher::with_default(&dispatch, || {
                runtime.block_on(async {
                    let (address, root, _) = metadata_server(vec![
                        ("/.well-known/oauth-authorization-server/tenant", metadata),
                        (
                            "/keys?tenant=private",
                            serde_json::json!({ "keys": [key] }).to_string(),
                        ),
                    ])
                    .await;
                    let _prepared = prepare(address, &root, None).await.unwrap();
                    address.port()
                })
            })
        });
        let samples = diagnostics.histograms.lock().unwrap();
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
        let spans = diagnostics.spans.lock().unwrap();
        let server = format!("server.address=\"{FIXTURE_HOST}\";server.port={port};");
        let client = format!(
            "span=authn_provider;otel.name=\"GET\";otel.kind=\"client\";http.request.method=\"GET\";{server}"
        );
        assert_eq!(
            *spans,
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
        use std::{future::Future, task::Poll, time::Duration};

        use crate::ProviderFailure;

        let diagnostics = Diagnostics::default();
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = tracing::Dispatch::new(diagnostics.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        metrics::with_local_recorder(&diagnostics, || {
            tracing::dispatcher::with_default(&dispatch, || {
                runtime.block_on(async {
                    // The listener never answers the TLS handshake.
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
                    std::future::poll_fn(|cx| {
                        assert!(pending.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
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
        let samples = diagnostics.histograms.lock().unwrap();
        let spans = diagnostics.spans.lock().unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(spans.len(), 2);
        for (index, outcome) in ["cancelled", "provider_timeout"].into_iter().enumerate() {
            assert!(
                samples[index]
                    .labels()
                    .any(|label| label.key() == "outcome" && label.value() == outcome)
            );
            assert!(spans[index].ends_with(&format!(
                "error.type=\"{outcome}\";otel.status_code=\"ERROR\";"
            )));
        }
    }
}
