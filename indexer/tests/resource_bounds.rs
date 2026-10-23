mod common;
use axum::{Json, Router, extract::State, routing::post};
use fossa_indexer::{
    App,
    auth::{Connections, Session},
    index::Record,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[test]
fn socket_limits_bound_wallets_and_real_peers_without_collapsing_proxied_users() {
    let limits = Arc::new(Connections::default());
    let local = Some("127.0.0.1".parse().unwrap());
    let connections = (0..40)
        .map(|wallet| limits.enter(&wallet.to_string(), local).unwrap())
        .collect::<Vec<_>>();
    let same = (0..8)
        .map(|_| limits.enter("same", local).unwrap())
        .collect::<Vec<_>>();
    assert!(limits.enter("same", local).is_err());
    drop(same);
    assert!(limits.enter("same", local).is_ok());
    let peer = Some("192.0.2.1".parse().unwrap());
    let direct = (0..32)
        .map(|wallet| limits.enter(&format!("direct:{wallet}"), peer).unwrap())
        .collect::<Vec<_>>();
    assert!(limits.enter("extra", peer).is_err());
    drop(direct);
    assert!(limits.enter("extra", peer).is_ok());
    drop(connections);
}

#[tokio::test]
async fn snapshot_requests_share_work_and_respect_advanced_floor() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = common::config(dir.path());
    let count = Arc::new(AtomicUsize::new(0));
    let state = count.clone();
    let router = Router::new().route("/", post(|State(count): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
        assert_eq!(body["method"], "getMultipleAccounts"); count.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let slot = body["params"][1]["minContextSlot"].as_u64().unwrap().max(10);
        Json(json!({"jsonrpc":"2.0","id":1,"result":{"context":{"slot":slot},"value":[null,null,null]}}))
    })).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.rpc_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let app = App::new(config).await.unwrap();
    let session = Session {
        wallet: solana_pubkey::Pubkey::new_from_array([31; 32]).to_string(),
        lane: solana_pubkey::Pubkey::new_from_array([32; 32]).to_string(),
        family: "fixture".into(),
        expires: u64::MAX,
    };
    let work = (0..16).map(|_| fossa_indexer::api::wallet::snapshot(&app, &session));
    for value in futures_util::future::join_all(work).await {
        assert_eq!(value.unwrap()["slot"], 10);
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    app.index.state.write().await.floor = 20;
    let (value, ()) = tokio::join!(
        fossa_indexer::api::wallet::snapshot(&app, &session),
        async {
            while count.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            app.index.state.write().await.floor = 30;
        }
    );
    assert_eq!(value.unwrap()["slot"], 30);
    assert_eq!(count.load(Ordering::SeqCst), 3);
    let mut other = session.clone();
    other.lane = solana_pubkey::Pubkey::new_from_array([33; 32]).to_string();
    fossa_indexer::api::wallet::snapshot(&app, &other)
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn receipt_drain_continues_through_slow_discovery_and_preserves_retry_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = common::config(dir.path());
    config.stale = Duration::from_secs(30);
    let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let router = Router::new()
        .route(
            "/",
            post(
                |State(calls): State<Arc<std::sync::Mutex<Vec<String>>>>,
                 Json(body): Json<Value>| async move {
                    let result = match body["method"].as_str().unwrap() {
                        "getSignaturesForAddress" => {
                            assert_eq!(body["params"][1]["commitment"], "finalized");
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            json!([])
                        }
                        "getTransaction" => {
                            assert_eq!(body["params"][1]["commitment"], "finalized");
                            calls
                                .lock()
                                .unwrap()
                                .push(body["params"][0].as_str().unwrap().to_owned());
                            Value::Null
                        }
                        _ => panic!("unexpected fixture RPC"),
                    };
                    Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
                },
            ),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.rpc_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let app = App::new(config).await.unwrap();
    let mut market = vec![0; 72];
    market[..8].copy_from_slice(b"FOSSAV20");
    market[40..72].copy_from_slice(app.config.mint.as_ref());
    let record = Record {
        slot: 10,
        version: 0,
        owner: app.config.program.to_string(),
        lamports: 1,
        data: market.into(),
    };
    app.index
        .apply(
            &app.db,
            &app.config,
            vec![(app.config.market.to_string(), record)],
            Some(10),
        )
        .await
        .unwrap();
    app.index.connected(true).await;
    app.index.activity().await;
    app.db.call(|conn| { let tx = conn.transaction()?; for index in 0..96 { tx.execute("INSERT INTO signatures(signature,slot) VALUES(?1,?2)", rusqlite::params![format!("fixture-{index}"), index])?; } tx.execute("INSERT INTO signatures(signature,slot,attempts,next_attempt) VALUES('due-retry',1,1,1)", [])?; tx.commit()?; Ok(()) }).await.unwrap();
    let worker = tokio::spawn(fossa_indexer::receipts::run(app.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let processed = app.db.call(|conn| Ok(conn.query_row("SELECT count(*) FROM signatures WHERE attempts=1 AND done=0 AND next_attempt>0", [], |row| row.get::<_, usize>(0))?)).await.unwrap();
            if processed == 96 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("queued receipts should drain without discovery delay");
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .position(|signature| signature == "due-retry")
            .unwrap()
            < 64,
        "due retries must run before unattempted backlog exhausts"
    );
    worker.abort();
    server.abort();
}
