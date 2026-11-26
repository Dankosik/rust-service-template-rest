use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use operation_context::{OperationContext, Stopped};
use serde::Serialize;
use tokio::sync::Notify;

use super::{
    SampleRow, base_options, encode_ch_str, encode_varuint, rbwnat_describe_id_column,
    spawn_http_server,
};
use crate::credentials::{ActiveCredentials, CredEpoch};
use crate::metadata::{
    CoordinatorPhase, LeaderId, MAX_TABLE_SLOTS, MetadataCoordinator, PrimeRole, SchemaGen,
    TableSlotRegistry,
};
use crate::{ClickHouse, CloseOutcome, InsertError, InsertFailure, InsertOptions, Row};

#[test]
fn metadata_coordinator_single_flight_and_cas_invalidation() {
    let mut coord = MetadataCoordinator::new();
    let gen1 = SchemaGen::INITIAL;
    let gen2 = gen1.next();
    let leader1 = LeaderId::new(1);
    let leader2 = LeaderId::new(2);
    assert_eq!(
        (coord.generation(), coord.phase()),
        (gen1, CoordinatorPhase::Cold)
    );

    assert_eq!(
        coord.acquire(),
        Some(PrimeRole::LeadPrime {
            generation: gen1,
            leader_id: leader1,
        })
    );
    assert_eq!(coord.acquire(), None);

    assert!(!coord.on_prime_populated(gen1, LeaderId::new(999)));
    assert!(!coord.on_prime_aborted(gen2, leader1));

    assert!(coord.on_prime_aborted(gen1, leader1));
    assert_eq!(
        (coord.generation(), coord.phase()),
        (gen1, CoordinatorPhase::Cold)
    );
    assert_eq!(
        coord.acquire(),
        Some(PrimeRole::LeadPrime {
            generation: gen1,
            leader_id: leader2,
        })
    );
    assert!(coord.on_prime_populated(gen1, leader2));
    assert_eq!(
        (coord.generation(), coord.phase()),
        (gen1, CoordinatorPhase::Warm)
    );
    assert_eq!(coord.acquire(), Some(PrimeRole::Warm(gen1)));

    assert!(coord.on_schema_mismatch(gen1));
    assert!(!coord.on_schema_mismatch(gen1));
    assert_eq!(
        (coord.generation(), coord.phase()),
        (gen2, CoordinatorPhase::Cold)
    );
    assert!(!coord.on_prime_populated(gen1, leader2));
}

#[test]
fn table_slot_registry_caps_lru_and_falls_back_when_all_active() {
    let creds = ActiveCredentials {
        cred_epoch: CredEpoch::INITIAL,
        read_client: clickhouse::Client::default().with_url("http://127.0.0.1:8123"),
        password: None,
    };
    let registry = TableSlotRegistry::new();

    let slot_0 = registry.get_or_create("t_0", &creds, "app");
    let weak_0 = Arc::downgrade(&slot_0);
    drop(slot_0);
    let slot_1 = registry.get_or_create("t_1", &creds, "app");
    let weak_1 = Arc::downgrade(&slot_1);
    drop(slot_1);
    let slot_2 = registry.get_or_create("t_2", &creds, "app");
    let weak_2 = Arc::downgrade(&slot_2);
    drop(slot_2);
    for i in 3..MAX_TABLE_SLOTS {
        let slot = registry.get_or_create(&format!("t_{i}"), &creds, "app");
        drop(slot);
    }
    assert_eq!(registry.len(), MAX_TABLE_SLOTS);

    let touched_0 = registry.get_or_create("t_0", &creds, "app");
    assert!(Arc::ptr_eq(
        &touched_0,
        &weak_0.upgrade().expect("t_0 live")
    ));
    drop(touched_0);

    drop(registry.get_or_create("evict_1", &creds, "app"));
    assert!(weak_0.upgrade().is_some(), "promoted t_0 must survive");
    assert!(weak_1.upgrade().is_none(), "LRU t_1 must be evicted");

    drop(registry.get_or_create("evict_2", &creds, "app"));
    assert!(weak_0.upgrade().is_some(), "shift removal preserves order");
    assert!(weak_2.upgrade().is_none(), "next LRU t_2 must be evicted");
    assert_eq!(registry.len(), MAX_TABLE_SLOTS);

    let mut pinned = Vec::with_capacity(MAX_TABLE_SLOTS);
    for i in 0..MAX_TABLE_SLOTS {
        pinned.push(registry.get_or_create(&format!("pinned_{i}"), &creds, "app"));
    }
    assert_eq!(registry.len(), MAX_TABLE_SLOTS);
    let ephemeral = registry.get_or_create("pinned_overflow", &creds, "app");
    assert_eq!(registry.len(), MAX_TABLE_SLOTS);
    drop(ephemeral);
    drop(pinned);
}

fn rbwnat_describe_id_and_extra_default() -> Bytes {
    let mut out = Vec::new();
    encode_varuint(7, &mut out);
    for col in [
        "name",
        "type",
        "default_type",
        "default_expression",
        "comment",
        "codec_expression",
        "ttl_expression",
    ] {
        encode_ch_str(col, &mut out);
    }
    for _ in 0..7 {
        encode_ch_str("String", &mut out);
    }
    encode_ch_str("id", &mut out);
    encode_ch_str("UInt64", &mut out);
    for _ in 0..5 {
        encode_ch_str("", &mut out);
    }
    encode_ch_str("extra", &mut out);
    encode_ch_str("UInt8", &mut out);
    encode_ch_str("DEFAULT", &mut out);
    encode_ch_str("0", &mut out);
    for _ in 0..3 {
        encode_ch_str("", &mut out);
    }
    Bytes::from(out)
}

#[derive(Debug, Serialize, Row)]
#[clickhouse(crate = "crate")]
struct EvolvedRow {
    id: u64,
    extra: u8,
}

struct EvolveServerState {
    describe_a: Arc<AtomicUsize>,
    describe_b: Arc<AtomicUsize>,
    a_leader_entered: Arc<Notify>,
    a_leader_release: Arc<Notify>,
    a_first_stall: Arc<AtomicUsize>,
    a_evolved: Arc<AtomicUsize>,
}

async fn spawn_evolve_metadata_server(
    state: Arc<EvolveServerState>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    spawn_http_server(move |req: Request<Incoming>| {
        let state = Arc::clone(&state);
        async move {
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if body_str.contains("DESCRIBE TABLE `table_a`") {
                state.describe_a.fetch_add(1, Ordering::SeqCst);
                if state.a_first_stall.swap(0, Ordering::SeqCst) == 1 {
                    state.a_leader_entered.notify_one();
                    state.a_leader_release.notified().await;
                }
                let resp = if state.a_evolved.load(Ordering::SeqCst) == 1 {
                    rbwnat_describe_id_and_extra_default()
                } else {
                    rbwnat_describe_id_column()
                };
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(resp))
                    .expect("resp");
            }
            if body_str.contains("DESCRIBE TABLE `table_b`") {
                state.describe_b.fetch_add(1, Ordering::SeqCst);
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::new()))
                .expect("resp")
        }
    })
    .await
}

#[tokio::test]
async fn single_flight_metadata_isolation_and_schema_drift_recovery() {
    let state = Arc::new(EvolveServerState {
        describe_a: Arc::new(AtomicUsize::new(0)),
        describe_b: Arc::new(AtomicUsize::new(0)),
        a_leader_entered: Arc::new(Notify::new()),
        a_leader_release: Arc::new(Notify::new()),
        a_first_stall: Arc::new(AtomicUsize::new(1)),
        a_evolved: Arc::new(AtomicUsize::new(0)),
    });
    let (addr, srv) = spawn_evolve_metadata_server(Arc::clone(&state)).await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let ch_leader = ch.clone();
    let ctx_leader = ctx.clone();
    let leader_handle = tokio::spawn(async move {
        ch_leader
            .insert(
                &ctx_leader,
                "prime_a",
                "table_a",
                &[SampleRow { id: 1 }],
                InsertOptions::batch(),
            )
            .await
    });
    state.a_leader_entered.notified().await;

    ch.insert(
        &ctx,
        "write_b",
        "table_b",
        &[SampleRow { id: 10 }],
        InsertOptions::batch(),
    )
    .await
    .expect("table_b must not block behind table_a");
    assert_eq!(state.describe_b.load(Ordering::SeqCst), 1);

    let mut waiters = Vec::new();
    for i in 2..=4_u64 {
        let ch_w = ch.clone();
        let ctx_w = ctx.clone();
        waiters.push(tokio::spawn(async move {
            ch_w.insert(
                &ctx_w,
                "wait_a",
                "table_a",
                &[SampleRow { id: i }],
                InsertOptions::batch(),
            )
            .await
        }));
    }
    tokio::task::yield_now().await;
    assert_eq!(state.describe_a.load(Ordering::SeqCst), 1);

    state.a_leader_release.notify_waiters();
    leader_handle.await.expect("join").expect("leader ok");
    for w in waiters {
        w.await.expect("join").expect("waiter ok");
    }
    assert_eq!(state.describe_a.load(Ordering::SeqCst), 1);

    state.a_evolved.store(1, Ordering::SeqCst);
    let report = ch
        .insert(
            &ctx,
            "drift_a_auto_reprime",
            "table_a",
            &[EvolvedRow { id: 5, extra: 9 }],
            InsertOptions::batch(),
        )
        .await
        .expect("stale warm slot transparently re-primes once before dispatch");
    assert_eq!(report.rows, 1);
    assert_eq!(state.describe_a.load(Ordering::SeqCst), 2);
    assert_eq!(state.describe_b.load(Ordering::SeqCst), 1);

    let report2 = ch
        .insert(
            &ctx,
            "recovered_a",
            "table_a",
            &[EvolvedRow { id: 6, extra: 7 }],
            InsertOptions::batch(),
        )
        .await
        .expect("re-primed write succeeds from warm cache");
    assert_eq!(report2.rows, 1);
    assert_eq!(state.describe_a.load(Ordering::SeqCst), 2);
    assert_eq!(state.describe_b.load(Ordering::SeqCst), 1);

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

#[tokio::test]
async fn metadata_leader_cancel_wakes_waiter_to_take_over() {
    let describe_calls = Arc::new(AtomicUsize::new(0));
    let first_entered = Arc::new(Notify::new());
    let first_stall = Arc::new(AtomicUsize::new(1));

    let calls_srv = Arc::clone(&describe_calls);
    let entered_srv = Arc::clone(&first_entered);
    let stall_srv = Arc::clone(&first_stall);
    let (addr, srv) = spawn_http_server(move |req: Request<Incoming>| {
        let calls = Arc::clone(&calls_srv);
        let entered = Arc::clone(&entered_srv);
        let stall = Arc::clone(&stall_srv);
        async move {
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if body_str.contains("DESCRIBE") {
                calls.fetch_add(1, Ordering::SeqCst);
                if stall.swap(0, Ordering::SeqCst) == 1 {
                    entered.notify_one();
                    std::future::pending::<()>().await;
                }
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::new()))
                .expect("resp")
        }
    })
    .await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let leader_ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let waiter_ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let ch_l = ch.clone();
    let l_ctx = leader_ctx.clone();
    let leader = tokio::spawn(async move {
        ch_l.insert(
            &l_ctx,
            "leader",
            "events",
            &[SampleRow { id: 1 }],
            InsertOptions::batch(),
        )
        .await
    });
    first_entered.notified().await;

    let ch_w = ch.clone();
    let waiter = tokio::spawn(async move {
        ch_w.insert(
            &waiter_ctx,
            "waiter",
            "events",
            &[SampleRow { id: 2 }],
            InsertOptions::batch(),
        )
        .await
    });
    tokio::task::yield_now().await;

    leader_ctx.cancel();
    assert_eq!(
        leader.await.expect("join"),
        Err(InsertError::NotApplied(InsertFailure::Stopped(
            Stopped::Cancelled
        )))
    );
    let waiter_res = waiter.await.expect("join").expect("waiter succeeds");
    assert_eq!(waiter_res.rows, 1);
    assert_eq!(describe_calls.load(Ordering::SeqCst), 2);

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}
