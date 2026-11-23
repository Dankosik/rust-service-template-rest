use std::path::Path;
use std::time::Duration;

use infra_temporal::worker::{
    TaskQueueOverrides, TemporalWorkerOptions, VersioningBehaviorMode, WorkerDeploymentMetadata,
    WorkerTunerOptions,
};
use infra_temporal::{
    ConnectError, TemporalAuth, TemporalConnection, TemporalMtlsOptions, TemporalOptions,
    TemporalStartup, TemporalTlsOptions, TlsRotationMode, Zeroizing,
};
use operation_context::{Deadline, OperationContext};
use secrecy::ExposeSecret as _;
use service_config::{
    TemporalClientConfig, TemporalTlsRotation, TemporalTunerConfig, TemporalVersioningBehavior,
    TemporalWorkerConfig, TemporalWorkerRuntimeConfig,
};

use crate::background::Background;

pub(super) fn client_options(
    temporal: &TemporalClientConfig,
    probe_budget: Duration,
) -> TemporalOptions {
    let auth = if let Some(api_key) = temporal.api_key() {
        TemporalAuth::ApiKey(Zeroizing::new(api_key.expose_secret().to_owned()))
    } else if let Some(path) = temporal.api_key_file() {
        TemporalAuth::ApiKeyFile(path.to_path_buf())
    } else {
        TemporalAuth::None
    };
    let mtls = match (temporal.client_cert_path(), temporal.client_key_path()) {
        (Some(client_cert_path), Some(client_key_path)) => Some(TemporalMtlsOptions {
            client_cert_path: client_cert_path.to_path_buf(),
            client_key_path: client_key_path.to_path_buf(),
            rotation: match temporal.mtls_rotation() {
                TemporalTlsRotation::Static => TlsRotationMode::Static,
                TemporalTlsRotation::Dynamic => TlsRotationMode::Dynamic,
            },
        }),
        _ => None,
    };
    let server_root_ca_path = temporal.root_ca_path().map(Path::to_path_buf);
    let tls = (server_root_ca_path.is_some() || mtls.is_some()).then_some(TemporalTlsOptions {
        server_root_ca_path,
        domain: None,
        mtls,
    });
    TemporalOptions {
        endpoint: temporal.endpoint().clone(),
        namespace: temporal.namespace().to_owned(),
        identity: temporal.identity().to_owned(),
        auth,
        tls,
        allow_plaintext: temporal.allow_plaintext(),
        allow_unauthenticated: temporal.allow_unauthenticated(),
        trusted_network: temporal.trusted_network(),
        credential_refresh_interval: temporal.credential_refresh_interval(),
        connect_timeout: temporal.connect_timeout(),
        rpc_timeout: temporal.rpc_timeout(),
        keep_alive_interval: temporal.keep_alive_interval(),
        keep_alive_timeout: temporal.keep_alive_timeout(),
        dns_load_balancing: temporal.dns_load_balancing(),
        http_proxy_url: temporal.http_proxy_url().cloned(),
        max_payload_bytes: temporal.max_payload_bytes(),
        probe_budget,
        payload_codec: None,
    }
}

pub(super) fn worker_options(cfg: &TemporalWorkerRuntimeConfig) -> TemporalWorkerOptions {
    let queues = cfg
        .queues()
        .iter()
        .map(|(name, queue)| {
            (
                name.clone(),
                TaskQueueOverrides {
                    max_cached_workflows: queue.max_cached_workflows,
                    workflow_pollers: queue.workflow_pollers,
                    activity_pollers: queue.activity_pollers,
                    max_worker_activities_per_second: queue.max_worker_activities_per_second,
                    tuner: queue.tuner.as_ref().map(convert_tuner),
                },
            )
        })
        .collect();
    TemporalWorkerOptions {
        task_queue: cfg.task_queue().map(str::to_owned),
        queues,
        max_cached_workflows: cfg.max_cached_workflows(),
        workflow_pollers: cfg.workflow_pollers(),
        activity_pollers: cfg.activity_pollers(),
        max_worker_activities_per_second: cfg.max_worker_activities_per_second(),
        tuner: convert_tuner(cfg.tuner()),
        graceful_shutdown_period: cfg.graceful_shutdown_period(),
    }
}

pub(super) fn deployment_metadata(cfg: &TemporalWorkerRuntimeConfig) -> WorkerDeploymentMetadata {
    let deployment = cfg.deployment();
    WorkerDeploymentMetadata {
        deployment_name: deployment.deployment_name.clone(),
        build_id: deployment.build_id.clone(),
        use_worker_versioning: deployment.use_worker_versioning,
        default_versioning_behavior: match deployment.default_versioning_behavior {
            TemporalVersioningBehavior::Unspecified => VersioningBehaviorMode::Unspecified,
            TemporalVersioningBehavior::Pinned => VersioningBehaviorMode::Pinned,
            TemporalVersioningBehavior::AutoUpgrade => VersioningBehaviorMode::AutoUpgrade,
        },
    }
}

const fn convert_tuner(tuner: &TemporalTunerConfig) -> WorkerTunerOptions {
    match *tuner {
        TemporalTunerConfig::ResourceBased {
            target_memory,
            target_cpu,
        } => WorkerTunerOptions::ResourceBased {
            target_memory,
            target_cpu,
        },
        TemporalTunerConfig::FixedSize {
            max_concurrent_workflow_tasks,
            max_concurrent_activities,
            max_concurrent_local_activities,
        } => WorkerTunerOptions::FixedSize {
            max_concurrent_workflow_tasks,
            max_concurrent_activities,
            max_concurrent_local_activities,
        },
    }
}

pub(super) async fn open(
    temporal: &TemporalClientConfig,
    config: &TemporalWorkerConfig,
    background: &Background,
    retained: &mut Option<TemporalStartup>,
) -> Result<TemporalConnection, ConnectError> {
    let options = client_options(temporal, config.health().probe_budget);
    let cancel = background.child_token();
    let context = OperationContext::new(
        Some(Deadline::after(options.connect_timeout)),
        cancel.clone(),
    );
    let connection = retained
        .insert(TemporalStartup::prepare(options, &context))
        .admit(cancel)
        .await?;
    tracing::info!(
        temporal.namespace = temporal.namespace(),
        "temporal_connected"
    );
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_and_client_options_preserve_config_values() {
        let cfg = service_config::test_support::temporal_worker(
            &[
                ("APP__APP__ENV", "local"),
                ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
                ("APP__TEMPORAL__ENABLED", "true"),
                ("APP__TEMPORAL__ENDPOINT", "https://temporal.example:7233"),
                ("APP__TEMPORAL__API_KEY", "secret-token"),
                ("APP__TEMPORAL_WORKER__TASK_QUEUE", "orders"),
                ("APP__TEMPORAL_WORKER__MAX_CACHED_WORKFLOWS", "250"),
                ("APP__TEMPORAL_WORKER__TUNER__MODE", "fixed_size"),
                (
                    "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_WORKFLOW_TASKS",
                    "4",
                ),
                (
                    "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_ACTIVITIES",
                    "8",
                ),
                (
                    "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_LOCAL_ACTIVITIES",
                    "6",
                ),
                (
                    "APP__TEMPORAL_WORKER__DEPLOYMENT__USE_WORKER_VERSIONING",
                    "true",
                ),
                (
                    "APP__TEMPORAL_WORKER__DEPLOYMENT__DEFAULT_VERSIONING_BEHAVIOR",
                    "pinned",
                ),
            ],
            crate::BUILD_INFO,
            crate::shutdown::TAIL,
        )
        .unwrap();

        let client_opts = client_options(cfg.temporal().unwrap(), cfg.health().probe_budget);
        assert!(client_opts.tls.is_none());
        std::assert_matches!(
            client_opts.auth,
            TemporalAuth::ApiKey(ref key) if key.as_str() == "secret-token"
        );

        let opts = worker_options(cfg.temporal_worker());
        assert_eq!(opts.task_queue.as_deref(), Some("orders"));
        assert_eq!(opts.max_cached_workflows, 250);
        assert_eq!(
            opts.tuner,
            WorkerTunerOptions::FixedSize {
                max_concurrent_workflow_tasks: 4,
                max_concurrent_activities: 8,
                max_concurrent_local_activities: 6,
            }
        );

        let deploy = deployment_metadata(cfg.temporal_worker());
        assert!(deploy.use_worker_versioning);
        assert_eq!(
            deploy.default_versioning_behavior,
            VersioningBehaviorMode::Pinned
        );
    }
}
