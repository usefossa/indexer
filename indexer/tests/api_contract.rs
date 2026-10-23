use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use fossa_indexer::{App, config::Config, protocol};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use std::{
    collections::HashMap,
    net::SocketAddr,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::broadcast, task::JoinHandle};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(Clone)]
struct Mock(Arc<MockState>);
struct MockState {
    records: Mutex<HashMap<String, Value>>,
    events: broadcast::Sender<String>,
    slot: AtomicU64,
    frozen_rpc: AtomicU64,
    minimum_requested: AtomicU64,
    fail_wallet: AtomicBool,
    online: AtomicBool,
    forwarded: AtomicU64,
}
impl Mock {
    fn account(&self, address: &str) -> Value {
        self.0
            .records
            .lock()
            .unwrap()
            .get(address)
            .cloned()
            .unwrap_or(Value::Null)
    }
    fn put(&self, address: &str, owner: &str, data: &[u8]) {
        self.0.records.lock().unwrap().insert(address.into(), json!({
            "owner":owner,"lamports":1_000_000,"data":[STANDARD.encode(data),"base64"],"executable":false,"rentEpoch":0,
        }));
        let _ = self.0.events.send(address.into());
    }
}
async fn mock_rpc(State(mock): State<Mock>, Json(body): Json<Value>) -> Json<Value> {
    let frozen = mock.0.frozen_rpc.load(Ordering::Relaxed);
    let slot = if frozen == 0 {
        mock.0.slot.load(Ordering::Relaxed)
    } else {
        frozen
    };
    let result = match body["method"].as_str().unwrap_or_default() {
        "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
        "getSlot" | "getBlockHeight" => json!(slot),
        "getProgramAccounts" => {
            mock.0.minimum_requested.store(
                body["params"][1]["minContextSlot"].as_u64().unwrap_or(0),
                Ordering::Relaxed,
            );
            json!({"context":{"slot":slot},"value":mock.0.records.lock().unwrap().iter()
            .filter(|(_,account)|account["owner"]==body["params"][0])
            .map(|(address,account)|json!({"pubkey":address,"account":account})).collect::<Vec<_>>()})
        }
        "getAccountInfo" => {
            json!({"context":{"slot":slot},"value":mock.account(body["params"][0].as_str().unwrap())})
        }
        "getMultipleAccounts" => {
            if mock.0.fail_wallet.load(Ordering::Relaxed) {
                return Json(
                    json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32000,"message":"Fixture wallet unavailable"}}),
                );
            }
            json!({"context":{"slot":slot},"value":body["params"][0].as_array().unwrap().iter()
            .map(|address|mock.account(address.as_str().unwrap())).collect::<Vec<_>>()})
        }
        "getLatestBlockhash" => {
            json!({"context":{"slot":slot},"value":{"blockhash":Pubkey::new_from_array([9;32]).to_string(),"lastValidBlockHeight":slot+150}})
        }
        "getMinimumBalanceForRentExemption" => json!(1000),
        "getFeeForMessage" => json!({"context":{"slot":slot},"value":5000}),
        "getSignaturesForAddress" => json!([]),
        "getTransaction" => Value::Null,
        "getSignatureStatuses" => json!({"context":{"slot":slot},"value":[null]}),
        "getHealth" => json!("ok"),
        "sendTransaction" => {
            mock.0.forwarded.fetch_add(1, Ordering::Relaxed);
            json!("unexpected")
        }
        _ => {
            return Json(
                json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32601,"message":"Unexpected mock method"}}),
            );
        }
    };
    Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
}
async fn mock_socket(State(mock): State<Mock>, ws: WebSocketUpgrade) -> Response {
    if !mock.0.online.load(Ordering::Relaxed) {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.on_upgrade(move |socket| upstream_stream(mock, socket))
}
async fn upstream_stream(mock: Mock, mut socket: WebSocket) {
    let mut events = mock.0.events.subscribe();
    let mut subscriptions = HashMap::<u64, String>::new();
    let mut timer = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            incoming=socket.recv()=>match incoming {
                Some(Ok(Message::Text(raw)))=>{
                    let Ok(body)=serde_json::from_str::<Value>(&raw) else {return};
                    let method=body["method"].as_str().unwrap_or_default();
                    let id=subscriptions.len() as u64+1;
                    subscriptions.insert(id,method.into());
                    if socket.send(Message::Text(json!({"jsonrpc":"2.0","id":body["id"],"result":id}).to_string().into())).await.is_err(){return;}
                },
                Some(Ok(Message::Ping(raw)))=>{if socket.send(Message::Pong(raw)).await.is_err(){return;}},
                Some(Ok(Message::Close(_)))|None|Some(Err(_))=>return,
                _=>{},
            },
            Ok(address)=events.recv()=>{
                let slot=mock.0.slot.load(Ordering::Relaxed);
                for (id,method) in &subscriptions {
                    if method=="programSubscribe" && socket.send(Message::Text(json!({"jsonrpc":"2.0","method":"programNotification","params":{"subscription":id,"result":{"context":{"slot":slot},"value":{"pubkey":address,"account":mock.account(&address)}}}}).to_string().into())).await.is_err(){return;}
                }
            },
            _=timer.tick()=>{
                if !mock.0.online.load(Ordering::Relaxed){let _=socket.close().await;return;}
                let slot=mock.0.slot.fetch_add(1,Ordering::Relaxed)+1;
                for (id,method) in &subscriptions {
                    if method=="slotSubscribe" && socket.send(Message::Text(json!({"jsonrpc":"2.0","method":"slotNotification","params":{"subscription":id,"result":{"slot":slot,"parent":slot-1,"root":slot-2}}}).to_string().into())).await.is_err(){return;}
                }
            },
        }
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    app: App,
    mock: Mock,
    base: String,
    owner: SigningKey,
    lane: Pubkey,
    booking: Pubkey,
    booking_data: Vec<u8>,
    tasks: Vec<JoinHandle<()>>,
    http: reqwest::Client,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_frozen_rpc(0).await
    }
    async fn with_frozen_rpc(frozen_rpc: u64) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock(Arc::new(MockState {
            records: Mutex::new(HashMap::new()),
            events: broadcast::channel(128).0,
            slot: AtomicU64::new(200),
            frozen_rpc: AtomicU64::new(frozen_rpc),
            minimum_requested: AtomicU64::new(0),
            fail_wallet: AtomicBool::new(false),
            online: AtomicBool::new(true),
            forwarded: AtomicU64::new(0),
        }));
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_router = Router::new()
            .route("/", get(mock_socket).post(mock_rpc))
            .with_state(mock.clone());
        let upstream_task = tokio::spawn(async move {
            axum::serve(upstream, upstream_router).await.unwrap();
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let base = format!("http://{bind}");
        let config = Config {
            rpc_url: format!("http://{upstream_address}/?token=fixture-private-rpc"),
            ws_url: Some(format!("ws://{upstream_address}/")),
            grpc_url: None,
            grpc_token: None,
            bind,
            public_url: base.clone(),
            database: directory.path().join("state/indexer.sqlite"),
            program: Pubkey::from_str("F8q85nw6UScZCBKBJqVAu7ZdYhmT9XRsm6TqZzJqXSNT").unwrap(),
            market: Pubkey::from_str("8wceBTFDAiC9WiT3xZGRf9Nf4xrz8eBCxExBtDWz1Som").unwrap(),
            mint: Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap(),
            genesis: "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d".into(),
            commitment: "confirmed".into(),
            reconcile: Duration::from_secs(1),
            stale: Duration::from_secs(5),
            max_accounts: 1000,
            max_rpc_bytes: 8 * 1024 * 1024,
            max_connections: 16,
            request_limit: 10000,
            auth_limit: 1000,
            access_secs: 3600,
            refresh_secs: 86400,
        };
        let owner = SigningKey::from_bytes(&[31; 32]);
        let owner_address = Pubkey::new_from_array(owner.verifying_key().to_bytes());
        let lane =
            Pubkey::new_from_array(SigningKey::from_bytes(&[42; 32]).verifying_key().to_bytes());
        let booking =
            Pubkey::new_from_array(SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes());
        let mut market_data = vec![0; 72];
        market_data[..8].copy_from_slice(b"FOSSAV20");
        market_data[8..40].copy_from_slice(owner_address.as_ref());
        market_data[40..72].copy_from_slice(config.mint.as_ref());
        mock.put(
            &config.market.to_string(),
            &config.program.to_string(),
            &market_data,
        );
        mock.put(
            &owner_address.to_string(),
            "11111111111111111111111111111111",
            &[],
        );
        let mut user = vec![0; 120];
        user[..8].copy_from_slice(b"USER0V20");
        user[8..40].copy_from_slice(owner_address.as_ref());
        user[40..72].copy_from_slice(config.market.as_ref());
        user[72..80].copy_from_slice(&123456u64.to_le_bytes());
        user[80..112].copy_from_slice(protocol::vault(&lane, &config).as_ref());
        user[112] = Pubkey::find_program_address(&[b"vault", lane.as_ref()], &config.program).1;
        user[113] = 7;
        mock.put(&lane.to_string(), &config.program.to_string(), &user);
        let mut token = vec![0; 165];
        token[..32].copy_from_slice(config.mint.as_ref());
        token[32..64].copy_from_slice(owner_address.as_ref());
        token[64..72].copy_from_slice(&9_007_199_254_740_993u64.to_le_bytes());
        token[108] = 1;
        mock.put(
            &protocol::associated(&owner_address, &config.mint).to_string(),
            protocol::TOKEN,
            &token,
        );
        let mut booking_data = vec![0; 368];
        booking_data[..8].copy_from_slice(b"BOOK0V20");
        booking_data[8..40].copy_from_slice(owner_address.as_ref());
        booking_data[72..104].copy_from_slice(config.market.as_ref());
        booking_data[148] = 5;
        booking_data[152..160].copy_from_slice(&7u64.to_le_bytes());
        booking_data[192..224].copy_from_slice(lane.as_ref());
        booking_data[256..288].copy_from_slice(owner_address.as_ref());
        mock.put(
            &booking.to_string(),
            &config.program.to_string(),
            &booking_data,
        );
        let app = App::new(config).await.unwrap();
        let mut tasks = app.start();
        tasks.push(upstream_task);
        let router = fossa_indexer::api::router(app.clone())
            .into_make_service_with_connect_info::<SocketAddr>();
        tasks.push(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        let fixture = Self {
            _directory: directory,
            app,
            mock,
            base,
            owner,
            lane,
            booking,
            booking_data,
            tasks,
            http: reqwest::Client::new(),
        };
        if frozen_rpc == 0 {
            fixture.ready(true).await;
        }
        fixture
    }
    async fn ready(&self, expected: bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(response) = self
                    .http
                    .get(format!("{}/v1/market/catalog", self.base))
                    .send()
                    .await
                    && let Ok(value) = response.json::<Value>().await
                    && value["ready"] == expected
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("index readiness transition");
    }
    async fn login(&self) -> Value {
        let identity = json!({"wallet":Pubkey::new_from_array(self.owner.verifying_key().to_bytes()).to_string(),"lane":self.lane.to_string()});
        let response = self
            .http
            .post(format!("{}/v1/opencode/challenge", self.base))
            .json(&identity)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "challenge status {}",
            response.status()
        );
        let challenge = response.json::<Value>().await.unwrap();
        assert_eq!(
            challenge["message"],
            format!(
                "fossa-opencode-connect-v1:{}:{}:{}:{}:{}",
                self.app.config.program,
                self.app.config.market,
                identity["wallet"].as_str().unwrap(),
                self.lane,
                challenge["challenge"].as_str().unwrap()
            )
        );
        let signature = self
            .owner
            .sign(challenge["message"].as_str().unwrap().as_bytes());
        let proof = json!({"challenge":challenge["challenge"],"signature":STANDARD.encode(signature.to_bytes())});
        let response = self
            .http
            .post(format!("{}/v1/opencode/connect", self.base))
            .json(&proof)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "connect status {}",
            response.status()
        );
        let session = response.json::<Value>().await.unwrap();
        assert!(
            !self
                .http
                .post(format!("{}/v1/opencode/connect", self.base))
                .json(&proof)
                .send()
                .await
                .unwrap()
                .status()
                .is_success(),
            "challenge replay accepted"
        );
        assert_eq!(session["wallet"], identity["wallet"]);
        assert_eq!(session["lane"], identity["lane"]);
        assert!(session["expires"].as_u64().unwrap() > fossa_indexer::now() * 1000);
        session
    }
    async fn authorized(&self, path: &str, token: &str) -> Value {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{} status {}",
            path,
            response.status()
        );
        response.json().await.unwrap()
    }
}

#[tokio::test]
async fn native_api_auth_wallet_precision_and_revocation() {
    let fixture = Fixture::new().await;
    let config = fixture
        .http
        .get(format!("{}/v1/config", fixture.base))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(!config.to_string().contains("fixture-private-rpc"));
    assert_eq!(config["programId"], fixture.app.config.program.to_string());
    assert!(config["opencode"]["maxInputTokens"].as_u64().unwrap() > 0);
    let session = fixture.login().await;
    let access = session["access"].as_str().unwrap();
    let wallet = fixture.authorized("/v1/opencode/wallet", access).await;
    assert_eq!(wallet["ready"], true);
    assert_eq!(wallet["freeAtomic"], "123456");
    assert_eq!(wallet["usdcAtomic"], "9007199254740993");
    assert_eq!(wallet["generation"], "7");
    assert_eq!(
        wallet["vault"],
        protocol::vault(&fixture.lane, &fixture.app.config).to_string()
    );
    assert!(
        wallet["bookings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|book| book["address"] == fixture.booking.to_string())
    );
    let funds = fixture.authorized("/v1/opencode/funds", access).await;
    assert_eq!(funds["free"], "123456");
    assert_eq!(funds["expired"], "0");
    assert!(fixture.authorized("/v1/opencode/receipts", access).await["receipts"].is_array());
    let refresh = fixture
        .http
        .post(format!("{}/v1/opencode/refresh", fixture.base))
        .bearer_auth(session["refresh"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert!(refresh.status().is_success());
    let rotated = refresh.json::<Value>().await.unwrap();
    assert_ne!(rotated["access"], session["access"]);
    assert_eq!(
        fixture
            .http
            .post(format!("{}/v1/opencode/refresh", fixture.base))
            .bearer_auth(session["refresh"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let logout = fixture
        .http
        .post(format!("{}/v1/opencode/logout", fixture.base))
        .bearer_auth(rotated["access"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert!(logout.status().is_success());
    assert_eq!(
        fixture
            .http
            .get(format!("{}/v1/opencode/wallet", fixture.base))
            .bearer_auth(rotated["access"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        fixture
            .http
            .post(format!("{}/v1/opencode/refresh", fixture.base))
            .bearer_auth(rotated["refresh"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

#[tokio::test]
async fn bootstrap_waits_until_rpc_snapshot_covers_stream_start() {
    let fixture = Fixture::with_frozen_rpc(1).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.mock.0.minimum_requested.load(Ordering::Relaxed) < 200 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot fenced by first stream slot");
    assert!(!fixture.app.index.ready(&fixture.app.config).await);
    assert_eq!(fixture.app.index.state.read().await.floor, 0);
    fixture.mock.0.frozen_rpc.store(0, Ordering::Relaxed);
    fixture.ready(true).await;
}

#[tokio::test]
async fn live_slots_cannot_keep_frozen_rpc_snapshot_ready() {
    let fixture = Fixture::new().await;
    let session = fixture.login().await;
    let floor = fixture.app.index.state.read().await.floor;
    fixture.mock.0.frozen_rpc.store(floor, Ordering::Relaxed);
    fixture.ready(false).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!fixture.app.index.ready(&fixture.app.config).await);
    assert_eq!(
        fixture
            .authorized("/v1/opencode/wallet", session["access"].as_str().unwrap())
            .await["ready"],
        true
    );
    fixture.mock.0.frozen_rpc.store(0, Ordering::Relaxed);
    fixture.ready(true).await;
}

#[tokio::test]
async fn wallet_read_failure_closes_live_instead_of_reporting_uninitialized_account() {
    let fixture = Fixture::new().await;
    let session = fixture.login().await;
    let mut request = format!("{}/v1/opencode/live", fixture.base.replace("http:", "ws:"))
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", session["access"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) =
                socket.next().await
            {
                let value: Value = serde_json::from_str(&raw).unwrap();
                if value["type"] == "wallet" {
                    assert_eq!(value["data"]["ready"], true);
                    break;
                }
            }
        }
    })
    .await
    .expect("initialized wallet live snapshot");
    fixture.mock.0.fail_wallet.store(true, Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) => {
                    let value: Value = serde_json::from_str(&raw).unwrap();
                    if value["type"] == "wallet" {
                        assert_ne!(value["data"]["ready"], false);
                    }
                }
                None
                | Some(Err(_))
                | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await
    .expect("failed wallet read closes live connection");
}

#[tokio::test]
async fn native_receipts_stream_all_compact_summaries() {
    let fixture = Fixture::new().await;
    let session = fixture.login().await;
    let owner = session["wallet"].as_str().unwrap().to_owned();
    let booking = fixture.booking.to_string();
    fixture.app.db.call(move |conn| {
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO receipts VALUES(?1,?2,?3,?4,?5)")?;
            for index in 0..1100 {
                let body = json!({"booking":booking,"generation":index.to_string(),"charge":"9007199254740993","extra":"not part of native summaries"});
                insert.execute(rusqlite::params![format!("{index:08}"), owner, "fixture-provider", 200, body.to_string()])?;
            }
            insert.execute(rusqlite::params!["other-owner", "other-owner", "fixture-provider", 200, json!({"booking":"private","generation":"1","charge":"1"}).to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let history = fixture
        .authorized("/v1/opencode/receipts", session["access"].as_str().unwrap())
        .await;
    let receipts = history["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1100);
    assert_eq!(receipts.first().unwrap()["generation"], "0");
    assert_eq!(receipts.last().unwrap()["generation"], "1099");
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.as_object().unwrap().len() == 3
                && receipt["charge"] == "9007199254740993")
    );
    assert_eq!(history["coverage"]["complete"], false);
}

#[tokio::test]
async fn live_wallet_quota_holds_for_connection_lifetime_and_releases_on_close() {
    let fixture = Fixture::new().await;
    let session = fixture.login().await;
    let mut request = format!("{}/v1/opencode/live", fixture.base.replace("http:", "ws:"))
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", session["access"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    let mut sockets = Vec::new();
    for _ in 0..8 {
        sockets.push(
            tokio_tungstenite::connect_async(request.clone())
                .await
                .unwrap()
                .0,
        );
    }
    match tokio_tungstenite::connect_async(request.clone()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 429)
        }
        other => panic!("expected wallet lifetime quota rejection: {other:?}"),
    }
    sockets.pop().unwrap().close(None).await.unwrap();
    let mut replacement = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok((socket, _)) = tokio_tungstenite::connect_async(request.clone()).await {
                break socket;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    replacement.close(None).await.unwrap();
    for mut socket in sockets {
        let _ = socket.close(None).await;
    }
}

#[tokio::test]
async fn live_wire_contract_and_rpc_denials_survive_reconnect() {
    let mut fixture = Fixture::new().await;
    let session = fixture.login().await;
    let access = session["access"].as_str().unwrap();
    let mut request = format!("{}/v1/opencode/live", fixture.base.replace("http:", "ws:"))
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {access}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"watch":fixture.booking.to_string()})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let mut kinds = std::collections::HashSet::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while kinds.len() < 4 {
            let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) = socket.next().await
            else {
                continue;
            };
            let value: Value = serde_json::from_str(&raw).unwrap();
            match value["type"].as_str().unwrap_or_default() {
                "catalog" => {
                    assert!(value["data"]["models"].is_array());
                    kinds.insert("catalog");
                }
                "wallet" => {
                    assert_eq!(value["data"]["lane"], fixture.lane.to_string());
                    kinds.insert("wallet");
                }
                "chain" if value["data"].is_object() => {
                    assert!(value["data"]["blockhash"].is_string());
                    assert!(value["data"]["bookingRent"].is_string());
                    assert!(value["data"]["signatureFee"].is_string());
                    kinds.insert("chain");
                }
                "booking" => {
                    assert_eq!(value["address"], fixture.booking.to_string());
                    assert_eq!(
                        value["data"]["owner"],
                        fixture.app.config.program.to_string()
                    );
                    assert_eq!(
                        STANDARD
                            .decode(value["data"]["data"].as_str().unwrap())
                            .unwrap(),
                        fixture.booking_data
                    );
                    kinds.insert("booking");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("native live envelopes");
    fixture.booking_data[152..160].copy_from_slice(&8u64.to_le_bytes());
    fixture.mock.put(
        &fixture.booking.to_string(),
        &fixture.app.config.program.to_string(),
        &fixture.booking_data,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) = socket.next().await
            else {
                continue;
            };
            let value: Value = serde_json::from_str(&raw).unwrap();
            if value["type"] == "booking"
                && value["data"]["data"].as_str().is_some_and(|raw| {
                    STANDARD
                        .decode(raw)
                        .is_ok_and(|data| protocol::u64_at(&data, 152) == 8)
                })
            {
                break;
            }
        }
    })
    .await
    .expect("booking update fanout");
    for body in [
        json!({"jsonrpc":"2.0","id":1,"method":"requestAirdrop","params":[session["wallet"],1]}),
        json!({"jsonrpc":"2.0","id":2,"method":"sendTransaction","params":["AA==",{"encoding":"base64"}]}),
    ] {
        assert!(
            !fixture
                .http
                .post(format!("{}/v1/opencode/rpc", fixture.base))
                .bearer_auth(access)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    assert_eq!(fixture.mock.0.forwarded.load(Ordering::Relaxed), 0);
    fixture.mock.0.online.store(false, Ordering::Relaxed);
    fixture.ready(false).await;
    fixture.mock.0.online.store(true, Ordering::Relaxed);
    fixture.ready(true).await;
    socket.close(None).await.unwrap();
}
