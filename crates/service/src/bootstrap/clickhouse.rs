use std::num::NonZeroU64;

use infra_clickhouse::{
    ClickHouse, ClickHouseOptions, ClientCertificate, QuerySettings, StartError,
};
use service_config::{ClickHouseConfig, ClickHouseQuerySettings};

pub(super) async fn open(clickhouse: &ClickHouseConfig) -> Result<ClickHouse, StartError> {
    let options = ClickHouseOptions {
        url: clickhouse.url.clone(),
        database: clickhouse.database.clone(),
        username: clickhouse.username.clone(),
        password: clickhouse.password.clone(),
        password_file: clickhouse.password_file.clone(),
        root_ca_path: clickhouse.root_ca_path.clone(),
        client_certificate: clickhouse.client_certificate.as_ref().map(|certificate| {
            ClientCertificate {
                cert_path: certificate.cert_path.clone(),
                key_path: certificate.key_path.clone(),
            }
        }),
        allow_plaintext: clickhouse.allow_plaintext,
        allow_unauthenticated: clickhouse.allow_unauthenticated,
        query_settings: match clickhouse.query_settings {
            ClickHouseQuerySettings::Client => QuerySettings::Client,
            ClickHouseQuerySettings::Server => QuerySettings::Server,
        },
        query_timeout: clickhouse.query_timeout,
        insert_timeout: clickhouse.insert_timeout,
        max_concurrency: clickhouse.max_concurrency.get(),
        max_result_rows: clickhouse.max_result_rows.get(),
        max_result_bytes: clickhouse.max_result_bytes.get(),
        http_response_buffer_size: clickhouse.http_response_buffer_size.get(),
        max_memory_usage: clickhouse.max_memory_usage.map(NonZeroU64::get),
        propagate_trace_context: clickhouse.propagate_trace_context,
    };
    ClickHouse::start(options).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::StatusCode;
    use infra_clickhouse::{
        AsyncDeduplication, AsyncInsertOptions, BatchInsertOptions, ClickHouse, CloseOutcome,
    };
    use infra_http::clickhouse::{insert_problem, query_problem};
    use infra_http::{Code, Problem, RequestContext};

    #[derive(Clone, serde::Deserialize, serde::Serialize, infra_clickhouse::Row)]
    #[clickhouse(crate = "infra_clickhouse::__clickhouse")]
    struct EventRow {
        id: u64,
        label: String,
    }

    #[test]
    fn a_clickhouse_handler_can_use_the_retained_startup_client_and_request_context() {
        let dependencies = super::super::shutdown::Dependencies::default();
        let clickhouse = dependencies.clickhouse;
        assert!(clickhouse.is_none());
        let _: axum::routing::MethodRouter = axum::routing::post(
            async |State(client): State<Option<ClickHouse>>,
                   request: RequestContext|
                   -> Result<StatusCode, Problem> {
                if let Some(client) = client {
                    let _row = client
                        .query(
                            "event_by_id",
                            "SELECT id, label FROM events WHERE id = {id:UInt64}",
                        )
                        .param("id", 1_u64)
                        .fetch_one::<EventRow>(request.operation())
                        .await
                        .map_err(|error| query_problem(error, || Problem::new(Code::NotFound)))?;
                    let rows = [EventRow {
                        id: 1,
                        label: "created".to_owned(),
                    }];
                    let _batch = request
                        .clickhouse_insert_batch(
                            &client,
                            "events_batch",
                            "events",
                            &rows,
                            BatchInsertOptions::new(),
                        )
                        .await
                        .map_err(insert_problem)?;
                    let _async_insert = request
                        .clickhouse_insert_async(
                            &client,
                            "events_async",
                            "events",
                            &rows,
                            AsyncInsertOptions::new()
                                .deduplication(AsyncDeduplication::ByToken("tok")),
                        )
                        .await
                        .map_err(insert_problem)?;
                }
                Ok(StatusCode::NO_CONTENT)
            },
        )
        .with_state(clickhouse);
    }

    #[tokio::test]
    async fn open_maps_clickhouse_config_and_closes_cleanly() {
        let config = super::super::test_config(&[
            ("APP__CLICKHOUSE__URL", "https://127.0.0.1:8443"),
            ("APP__CLICKHOUSE__DATABASE", "app"),
            ("APP__CLICKHOUSE__USERNAME", "app"),
            ("APP__CLICKHOUSE__PASSWORD", "unit-test-pw"),
            ("APP__CLICKHOUSE__QUERY_SETTINGS", "server"),
            ("APP__CLICKHOUSE__MAX_MEMORY_USAGE", "0"),
        ]);
        let settings = config.clickhouse().expect("configured clickhouse");
        assert_eq!(
            settings.query_settings,
            service_config::ClickHouseQuerySettings::Server
        );
        assert!(settings.max_memory_usage.is_none());
        let client = super::open(settings).await.expect("open clickhouse");
        assert_eq!(
            client.close(Duration::from_secs(1)).await,
            CloseOutcome::Complete
        );
    }
}
