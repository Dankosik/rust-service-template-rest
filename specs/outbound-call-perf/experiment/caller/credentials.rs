use std::{fs, future::poll_fn, pin::Pin, task::Poll, time::Duration};

use bytes::Bytes;
use http::{Request, Uri};
use infra_oauth2_client_credentials::{
    AcquisitionError, Algorithm, AuthenticatedClient, Error, ExchangeTokenTarget, OnBehalfOf,
    Options, Registration, ServiceTokenTarget,
};
use infra_outbound_http::{Client, Error as HttpError, Limits, Url};
use operation_context::OperationContext;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::{sync::oneshot, time::Instant};

use crate::{Config, Observation, Result, control, measure_with};

const REUSE_MARGIN: Duration = Duration::from_secs(10);
const FETCH_CUTOFF: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const RESOURCE_TIMEOUT: Duration = Duration::from_secs(8);
const BODY: &[u8] = &[b'a'; 1024];

pub(super) fn registration(config: &Config) -> Result<Registration> {
    let origin = format!("https://{}", config.authority(config.http_port));
    Ok(Registration::prepare(Options {
        token_url: format!("{origin}/token"),
        assertion_audience: format!("{origin}/issuer"),
        client_id: "fixture-client".into(),
        key_id: "fixture-key".into(),
        algorithm: Algorithm::Rs256,
        private_key: SecretString::from(fs::read_to_string(
            config.ca.with_file_name("client.key"),
        )?),
        provider_concurrency: 64,
    })?)
}

#[derive(Clone)]
struct Resource {
    client: AuthenticatedClient,
    uri: Uri,
    exchange: bool,
}

impl Resource {
    async fn hit(&self) -> Observation {
        self.call(&OperationContext::with_timeout(RESOURCE_TIMEOUT))
            .await
    }

    async fn call(&self, context: &OperationContext) -> Observation {
        let mut request = Request::new(Bytes::new());
        *request.uri_mut() = self.uri.clone();
        if self.exchange {
            request
                .extensions_mut()
                .insert(OnBehalfOf::new(SecretString::from("fixture-subject")));
        }
        match self.client.execute_with_context(request, context).await {
            Ok(response)
                if response.status().as_u16() == 200 && response.body().as_ref() == BODY =>
            {
                Observation::success(200, response.body())
            }
            Ok(response) => Observation::failure("failed", Some(response.status().as_u16())),
            Err(
                Error::Acquisition(AcquisitionError::Timeout) | Error::Resource(HttpError::Timeout),
            ) => Observation::failure("timed_out", None),
            Err(
                Error::Acquisition(AcquisitionError::Cancelled)
                | Error::Resource(HttpError::Cancelled),
            ) => Observation::failure("cancelled", None),
            Err(Error::Acquisition(AcquisitionError::Unavailable)) => {
                Observation::failure("unavailable", None)
            }
            Err(
                Error::AuthorizationConflict
                | Error::SubjectRequired
                | Error::UnexpectedSubject
                | Error::Resource(HttpError::InvalidTarget),
            ) => Observation::failure("refused", None),
            Err(_) => Observation::failure("failed", None),
        }
    }

    async fn window(&self, count: usize, concurrency: usize) -> Result<Value> {
        measure_with(count, concurrency, self.clone(), |resource, _| async move {
            let observation = resource.hit().await;
            (resource, observation)
        })
        .await
    }
}

pub(super) async fn run(config: &Config) -> Result<Value> {
    if !(18..=120).contains(&config.ttl) {
        return Err("credential scenarios require TTL from 18 through 120 seconds".into());
    }
    if matches!(
        config.scenario.as_str(),
        "service-before" | "service-refresh"
    ) && !matches!(config.concurrency, 1 | 64)
    {
        return Err("service hit scenarios require 1 or 64 callers".into());
    }

    let registration = registration(config)?;
    let origin = format!("https://{}", config.authority(config.http_port));
    let origin_uri: Uri = origin.parse()?;
    let uri = format!("{origin}/one").parse()?;
    let transport = Client::try_new(
        &Url::parse(&origin)?,
        Limits {
            operation_timeout: RESOURCE_TIMEOUT,
            response_header_count: 64,
            response_body_bytes: 4096,
        },
    )?;

    let reuse = Duration::from_secs(config.ttl) - REUSE_MARGIN;
    if config.scenario == "exchange" {
        let target = ExchangeTokenTarget::try_new(&[], Some("fixture-resource".into()))?;
        let tokens = registration.exchange_tokens(target, origin_uri, 1)?;
        let resource = Resource {
            client: tokens.http(transport)?,
            uri,
            exchange: true,
        };
        let setup_started = Instant::now();
        let (_, before) = warm(
            config,
            &resource,
            "exchange_grants",
            "exchange_token_requests",
        )
        .await?;
        return hits(
            config,
            &resource,
            before,
            "exchange_token_requests",
            setup_started + reuse,
            false,
        )
        .await;
    }

    let (tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), origin_uri.clone());
    let resource = Resource {
        client: tokens.http(transport.clone())?,
        uri: uri.clone(),
        exchange: false,
    };
    let (shutdown, stop) = oneshot::channel();
    let driver_task = tokio::spawn(driver.run(async {
        let _ = stop.await;
    }));
    let setup_started = Instant::now();
    let (issued_before, before) =
        warm(config, &resource, "service_grants", "old_token_requests").await?;
    let result = match config.scenario.as_str() {
        "service-before" => {
            hits(
                config,
                &resource,
                before,
                "old_token_requests",
                setup_started + reuse * 3 / 4,
                false,
            )
            .await
        }
        "service-refresh" => {
            active_refresh(
                config,
                &resource,
                before,
                setup_started + reuse,
                issued_before + reuse * 31 / 40,
            )
            .await
        }
        "service-demand" => {
            tokio::time::sleep_until(issued_before + reuse).await;
            let wait = demand(config, &resource).await?;
            let _ = shutdown.send(());
            driver_task.await?;
            require_outcome(&resource.hit().await, "unavailable")?;
            let after_driver_shutdown = stats(config)?;
            check_delta(&wait["after"], &after_driver_shutdown, "service_grants", 0)?;
            check_delta(&wait["after"], &after_driver_shutdown, "resources", 0)?;
            let closed =
                registration_closure(config, registration, origin_uri, transport, uri).await?;
            return Ok(json!({"windows": [], "demand": wait,
                "after_driver_shutdown": after_driver_shutdown, "driver_shutdown_outcome": "unavailable",
                "registration_closure": closed}));
        }
        _ => Err("unknown credential scenario".into()),
    };
    drop(registration);
    driver_task.await?;
    result
}

async fn warm(
    config: &Config,
    resource: &Resource,
    grant: &str,
    bearer: &str,
) -> Result<(Instant, Value)> {
    let initial = stats(config)?;
    if counter(&initial, "token_ttl")? != config.ttl {
        return Err("fixture token TTL differs from caller TTL".into());
    }
    require_outcome(&resource.hit().await, "successful")?;
    let issued_before = Instant::now();
    let count = config.warmup.max(config.concurrency);
    let warm = resource.window(count, config.concurrency).await?;
    require_successes(&warm, count)?;
    let after = stats(config)?;
    check_delta(&initial, &after, grant, 1)?;
    let warmed = u64::try_from(count)? + 1;
    check_delta(&initial, &after, bearer, warmed)?;
    check_delta(&initial, &after, "resources", warmed)?;
    Ok((issued_before, after))
}

async fn active_refresh(
    config: &Config,
    resource: &Resource,
    setup: Value,
    reuse_cutoff: Instant,
    refresh_due: Instant,
) -> Result<Value> {
    control(config, "POST", "/gate/hold")?;
    tokio::time::sleep_until(refresh_due).await;
    let triggered = Instant::now();
    let cutoff = reuse_cutoff.min(triggered + FETCH_CUTOFF);
    require_outcome(&resource.hit().await, "successful")?;
    let pending = wait_for_pending(config, cutoff).await?;
    check_delta(&setup, &pending, "service_grants", 1)?;
    check_delta(&setup, &pending, "service_responses", 0)?;
    check_delta(&setup, &pending, "old_token_requests", 1)?;
    check_delta(&setup, &pending, "resources", 1)?;
    let result = hits(
        config,
        resource,
        pending.clone(),
        "old_token_requests",
        cutoff,
        true,
    )
    .await;
    let released = control(config, "POST", "/gate/release");
    let mut result = result?;
    result["after_setup"] = setup;
    if let Err(error) = released {
        result["valid"] = json!(false);
        result["gate_release_error"] = json!(error.to_string());
        return Ok(result);
    }
    if result["valid"] != true {
        return Ok(result);
    }
    match replacement_published(config, resource, &pending, triggered + FETCH_CUTOFF).await {
        Ok(published) => result["after_replacement_publication"] = published,
        Err(error) => {
            result["valid"] = json!(false);
            result["validation_error"] = json!(error.to_string());
        }
    }
    Ok(result)
}

async fn replacement_published(
    config: &Config,
    resource: &Resource,
    pending: &Value,
    published_deadline: Instant,
) -> Result<Value> {
    loop {
        require_outcome(&resource.hit().await, "successful")?;
        let published = stats(config)?;
        if Instant::now() >= published_deadline {
            return Err("replacement failed to publish before the provider cutoff".into());
        }
        if counter(&published, "replacement_token_requests")?
            > counter(pending, "replacement_token_requests")?
        {
            check_delta(pending, &published, "service_grants", 0)?;
            check_delta(pending, &published, "service_responses", 1)?;
            return Ok(published);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn hits(
    config: &Config,
    resource: &Resource,
    before: Value,
    bearer: &str,
    cutoff: Instant,
    pending: bool,
) -> Result<Value> {
    let opening = Instant::now();
    if opening >= cutoff {
        return Err("credential window opened after its eligibility cutoff".into());
    }
    let window = resource.window(config.count, config.concurrency).await?;
    let closing = Instant::now();
    let after = stats(config);
    let validation = (|| -> Result<()> {
        if closing >= cutoff {
            return Err("credential window crossed its eligibility cutoff".into());
        }
        let after = after.as_ref().map_err(ToString::to_string)?;
        let count = u64::try_from(config.count)?;
        require_successes(&window, config.count)?;
        check_delta(&before, after, "resources", count)?;
        check_delta(&before, after, bearer, count)?;
        for key in [
            "service_grants",
            "exchange_grants",
            "service_responses",
            "exchange_responses",
            "unauthorized",
        ] {
            check_delta(&before, after, key, 0)?;
        }
        let expected_pending = u64::from(pending);
        if counter(&before, "service_pending")? != expected_pending
            || counter(after, "service_pending")? != expected_pending
            || before["gate_held"].as_bool() != Some(pending)
            || after["gate_held"].as_bool() != Some(pending)
        {
            return Err("unexpected gate state across the credential measurement".into());
        }
        Ok(())
    })();
    Ok(
        json!({"windows": [window], "before": before, "after": after.ok(),
        "valid": validation.is_ok(),
        "validation_error": validation.err().map(|error| error.to_string()),
        "cutoff_remaining_before_ns": (cutoff - opening).as_nanos(),
        "cutoff_remaining_after_ns": cutoff.saturating_duration_since(closing).as_nanos()}),
    )
}

async fn demand(config: &Config, resource: &Resource) -> Result<Value> {
    control(config, "POST", "/gate/hold")?;
    let before = stats(config)?;
    let leader = resource.clone();
    let started = Instant::now();
    let leader = tokio::spawn(async move { leader.hit().await });
    let pending = wait_for_pending(config, started + FETCH_CUTOFF).await?;
    let context = OperationContext::with_timeout(RESOURCE_TIMEOUT);
    let mut waiters = Vec::new();
    for _ in 1..config.concurrency.max(2) {
        let mut waiter = Box::pin(resource.call(&context));
        require_pending(waiter.as_mut()).await?;
        waiters.push(waiter);
    }

    let cancelled_context = OperationContext::with_timeout(RESOURCE_TIMEOUT);
    let mut cancelled = Box::pin(resource.call(&cancelled_context));
    require_pending(cancelled.as_mut()).await?;
    cancelled_context.cancel();
    require_outcome(&cancelled.await, "cancelled")?;
    let timeout_context = OperationContext::with_timeout(Duration::from_millis(100));
    let mut timeout = Box::pin(resource.call(&timeout_context));
    require_pending(timeout.as_mut()).await?;
    require_outcome(&timeout.await, "timed_out")?;

    let waiting = stats(config)?;
    check_delta(&before, &waiting, "service_grants", 1)?;
    check_delta(&before, &waiting, "service_responses", 0)?;
    check_delta(&before, &waiting, "resources", 0)?;
    if leader.is_finished()
        || Instant::now() >= started + FETCH_CUTOFF
        || counter(&waiting, "service_pending")? != 1
    {
        return Err("replacement was no longer pending while observing demand waiters".into());
    }
    control(config, "POST", "/gate/release")?;
    let count = u64::try_from(waiters.len())? + 1;
    require_outcome(&leader.await?, "successful")?;
    for waiter in waiters {
        require_outcome(&waiter.await, "successful")?;
    }
    let after = stats(config)?;
    check_delta(&before, &after, "service_grants", 1)?;
    check_delta(&before, &after, "service_responses", 1)?;
    check_delta(&before, &after, "resources", count)?;
    check_delta(&before, &after, "replacement_token_requests", count)?;
    Ok(
        json!({"before": before, "provider_observed_pending": pending,
        "before_release": waiting, "after": after, "successful": count, "cancelled": 1, "timed_out": 1}),
    )
}

async fn registration_closure(
    config: &Config,
    registration: Registration,
    origin: Uri,
    transport: Client,
    uri: Uri,
) -> Result<Value> {
    let (tokens, driver) = registration.service_tokens(ServiceTokenTarget::default(), origin);
    let resource = Resource {
        client: tokens.http(transport)?,
        uri,
        exchange: false,
    };
    let driver = tokio::spawn(driver.run(std::future::pending()));
    control(config, "POST", "/gate/hold")?;
    let before = stats(config)?;
    let call = tokio::spawn(async move { resource.hit().await });
    let pending = wait_for_pending(config, Instant::now() + FETCH_CUTOFF).await?;
    drop(registration);
    require_outcome(&call.await?, "unavailable")?;
    driver.await?;
    let closed = stats(config)?;
    control(config, "POST", "/gate/release")?;
    check_delta(&before, &closed, "service_grants", 1)?;
    check_delta(&before, &closed, "service_responses", 0)?;
    check_delta(&before, &closed, "resources", 0)?;
    Ok(
        json!({"before": before, "provider_observed_pending": pending, "after": closed, "outcome": "unavailable"}),
    )
}

async fn require_pending<T>(mut future: Pin<&mut impl Future<Output = T>>) -> Result<()> {
    let pending = poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await;
    if !pending {
        return Err("demand call completed while the replacement grant was held".into());
    }
    Ok(())
}

async fn wait_for_pending(config: &Config, cutoff: Instant) -> Result<Value> {
    while Instant::now() < cutoff {
        let current = stats(config)?;
        if counter(&current, "service_pending")? == 1
            && current["gate_held"].as_bool() == Some(true)
            && Instant::now() < cutoff
        {
            return Ok(current);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Err("fixture did not observe a held service grant before cutoff".into())
}

fn stats(config: &Config) -> Result<Value> {
    control(config, "GET", "/stats")
}

fn counter(stats: &Value, key: &str) -> Result<u64> {
    stats[key]
        .as_u64()
        .ok_or_else(|| format!("fixture receipt lacks counter {key}").into())
}

fn check_delta(before: &Value, after: &Value, key: &str, expected: u64) -> Result<()> {
    let start = counter(before, key)?;
    let end = counter(after, key)?;
    if end.checked_sub(start) != Some(expected) {
        return Err(format!("fixture counter {key} started at {start}, ended at {end}, and expected an increase of {expected}").into());
    }
    Ok(())
}

fn require_outcome(observation: &Observation, expected: &str) -> Result<()> {
    if observation.outcome != expected {
        return Err(format!(
            "credential call returned {} (HTTP {:?}), expected {expected}",
            observation.outcome, observation.status
        )
        .into());
    }
    Ok(())
}

fn require_successes(window: &Value, count: usize) -> Result<()> {
    let samples = window["samples"]
        .as_array()
        .ok_or("credential window lacks observations")?;
    if samples.len() != count
        || samples
            .iter()
            .any(|sample| sample["outcome"] != "successful")
    {
        return Err(
            "credential window did not return the expected status and body for every offered call"
                .into(),
        );
    }
    Ok(())
}
