mod common;

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use fossa_indexer::{App, auth, index::Record, protocol};
use serde_json::{Value, json};
use solana_message::{
    Message, MessageHeader, VersionedMessage, compiled_instruction::CompiledInstruction, v0,
};
use solana_transaction::versioned::VersionedTransaction;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    app: App,
    requests: Arc<Mutex<Vec<Value>>>,
    upstream: tokio::task::JoinHandle<()>,
    payer: SigningKey,
    access: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.upstream.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut config = common::config(directory.path());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler =
            Router::new()
                .route(
                    "/",
                    post(
                        |State(requests): State<Arc<Mutex<Vec<Value>>>>,
                         Json(body): Json<Value>| async move {
                            requests.lock().unwrap().push(body.clone());
                            Json(json!({"jsonrpc":"2.0","id":body["id"],"result":"synthetic"}))
                        },
                    ),
                )
                .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.rpc_url = format!("http://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            axum::serve(listener, handler).await.unwrap();
        });
        let app = App::new(config).await.unwrap();
        let mut market = vec![0; 248];
        market[..8].copy_from_slice(&protocol::MARKET_DISCRIMINATOR);
        market[40..72].copy_from_slice(app.config.mint.as_ref());
        {
            let mut state = app.index.state.write().await;
            state.connected = true;
            state.reconciled = Some(Instant::now());
            state.activity = Some(Instant::now());
            state.accounts.insert(
                app.config.market.to_string(),
                Record {
                    slot: 10,
                    version: 0,
                    owner: app.config.program.to_string(),
                    lamports: 1,
                    data: market.into(),
                },
            );
        }
        let payer = SigningKey::from_bytes(&[81; 32]);
        let wallet = bs58::encode(payer.verifying_key().to_bytes()).into_string();
        let access = "a".repeat(64);
        let hash = auth::digest(&access);
        app.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO sessions VALUES(?1,'synthetic',0,?2,?2,?3)",
                    rusqlite::params![hash, wallet, fossa_indexer::now() + 3600],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        Self {
            _directory: directory,
            app,
            requests,
            upstream,
            payer,
            access,
        }
    }

    fn transaction(&self, versioned: bool) -> VersionedTransaction {
        let header = MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 1,
        };
        let account_keys = vec![
            self.payer.verifying_key().to_bytes().into(),
            self.app.config.program.to_bytes().into(),
        ];
        let instructions = vec![CompiledInstruction {
            program_id_index: 1,
            accounts: if versioned { vec![0, 2] } else { vec![0] },
            data: protocol::RESERVE_LIST_BOOKING_DISCRIMINATOR.to_vec(),
        }];
        let message = if versioned {
            VersionedMessage::V0(v0::Message {
                header,
                account_keys,
                recent_blockhash: Default::default(),
                instructions,
                address_table_lookups: vec![v0::MessageAddressTableLookup {
                    account_key: [8; 32].into(),
                    writable_indexes: vec![1],
                    readonly_indexes: vec![],
                }],
            })
        } else {
            VersionedMessage::Legacy(Message {
                header,
                account_keys,
                recent_blockhash: Default::default(),
                instructions,
            })
        };
        let mut tx = VersionedTransaction {
            signatures: vec![Default::default()],
            message,
        };
        self.sign(&mut tx);
        tx
    }

    fn sign(&self, tx: &mut VersionedTransaction) {
        tx.signatures[0] = self
            .payer
            .sign(&bincode::serialize(&tx.message).unwrap())
            .to_bytes()
            .into();
    }

    fn body(&self, tx: &VersionedTransaction, method: &str, options: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":[STANDARD.encode(bincode::serialize(tx).unwrap()),options]})
    }

    async fn request(&self, body: &Value, authenticated: bool) -> (StatusCode, Value) {
        let mut request =
            Request::post("/v1/opencode/rpc").header("content-type", "application/json");
        if authenticated {
            request = request.header("authorization", format!("Bearer {}", self.access));
        }
        let response = fossa_indexer::api::router(self.app.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
}

#[tokio::test]
async fn finalized_epoch_proofs_require_auth_and_bounded_context_options() {
    let fixture = Fixture::new().await;
    let valid = json!({"jsonrpc":"2.0","id":1,"method":"getEpochInfo","params":[{"commitment":"finalized","minContextSlot":10}]});
    assert_eq!(
        fixture.request(&valid, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.request(&valid, true).await.0, StatusCode::OK);
    for params in [
        json!([{"commitment":"root"}]),
        json!([{"minContextSlot":-1}]),
        json!([{"encoding":"base64"}]),
        json!([null]),
        json!([{}, {}]),
    ] {
        let mut body = valid.clone();
        body["params"] = params;
        assert_eq!(
            fixture.request(&body, true).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(*fixture.requests.lock().unwrap(), vec![valid]);
}

#[tokio::test]
async fn blockhash_expiry_proofs_require_auth_and_validate_hash_and_options() {
    let fixture = Fixture::new().await;
    let hash = fixture.app.config.market.to_string();
    let valid = json!({"jsonrpc":"2.0","id":1,"method":"isBlockhashValid","params":[hash,{"commitment":"finalized","minContextSlot":10}]});
    assert_eq!(
        fixture.request(&valid, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    for params in [
        json!([]),
        json!(["invalid"]),
        json!(["a".repeat(100)]),
        json!([hash, null]),
        json!([hash, {"commitment":"root"}]),
        json!([hash, {"minContextSlot":-1}]),
        json!([hash, {"minContextSlot":1.5}]),
        json!([hash, {"encoding":"base64"}]),
        json!([hash, {}, {}]),
    ] {
        let mut body = valid.clone();
        body["params"] = params;
        assert_eq!(
            fixture.request(&body, true).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
    fixture.app.index.connected(false).await;
    assert_eq!(fixture.request(&valid, true).await.0, StatusCode::OK);
    assert_eq!(*fixture.requests.lock().unwrap(), vec![valid]);
}

#[tokio::test]
async fn native_legacy_and_v0_messages_relay_exact_bytes_without_lookup_round_trips() {
    let fixture = Fixture::new().await;
    for versioned in [false, true] {
        let tx = fixture.transaction(versioned);
        let send = fixture.body(
            &tx,
            "sendTransaction",
            json!({"encoding":"base64","skipPreflight":true,"maxRetries":0}),
        );
        assert_eq!(fixture.request(&send, true).await.0, StatusCode::OK);
        let verified = fixture.body(&tx, "simulateTransaction", json!({"encoding":"base64","sigVerify":true,"commitment":"processed","minContextSlot":10}));
        assert_eq!(fixture.request(&verified, true).await.0, StatusCode::OK);
        let mut unsigned = tx;
        unsigned.signatures.fill(Default::default());
        let probe = fixture.body(&unsigned, "simulateTransaction", json!({"encoding":"base64","sigVerify":false,"replaceRecentBlockhash":true,"commitment":"processed","minContextSlot":10}));
        assert_eq!(fixture.request(&probe, true).await.0, StatusCode::OK);
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(&requests[requests.len() - 3..], &[send, verified, probe]);
    }
    assert_eq!(fixture.requests.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn relay_rejects_wrong_payer_signatures_loaded_programs_and_malformed_v0_frames() {
    let fixture = Fixture::new().await;
    let good = fixture.transaction(true);
    let options = json!({"encoding":"base64"});
    assert_eq!(
        fixture
            .request(
                &fixture.body(&good, "sendTransaction", options.clone()),
                false
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let mut invalid = Vec::new();
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.header.num_required_signatures = 2;
        message.account_keys.insert(
            1,
            SigningKey::from_bytes(&[82; 32])
                .verifying_key()
                .to_bytes()
                .into(),
        );
        message.instructions[0].program_id_index = 2;
        message.instructions[0].accounts = vec![0, 1, 3];
    }
    tx.signatures.push(Default::default());
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    tx.signatures[0] = [0; 64].into();
    invalid.push(tx);
    let mut tx = good.clone();
    tx.signatures[0] = [1; 64].into();
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.account_keys[0] = [82; 32].into();
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.account_keys[1] = [83; 32].into();
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.instructions[0].program_id_index = 2;
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.instructions[0].accounts.push(250);
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.account_keys.push(message.account_keys[0]);
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.address_table_lookups[0].readonly_indexes.push(1);
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message
            .address_table_lookups
            .push(message.address_table_lookups[0].clone());
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.address_table_lookups[0].readonly_indexes = (0..=254).collect();
    }
    fixture.sign(&mut tx);
    invalid.push(tx);
    let mut tx = good.clone();
    if let VersionedMessage::V0(message) = &mut tx.message {
        message.instructions[0].data.push(7);
    }
    invalid.push(tx);
    for tx in invalid {
        assert!(
            fixture
                .request(&fixture.body(&tx, "sendTransaction", options.clone()), true)
                .await
                .0
                .is_client_error()
        );
    }
    let mut bytes = bincode::serialize(&good).unwrap();
    assert_eq!(bytes[65], 128);
    bytes[65] = 129;
    let mut unknown = fixture.body(&good, "sendTransaction", options.clone());
    unknown["params"][0] = json!(STANDARD.encode(bytes));
    assert_eq!(
        fixture.request(&unknown, true).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut trailing = bincode::serialize(&good).unwrap();
    trailing.push(0);
    unknown["params"][0] = json!(STANDARD.encode(trailing));
    assert_eq!(
        fixture.request(&unknown, true).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn simulations_require_explicit_signature_policy_and_never_admit_unsigned_submission() {
    let fixture = Fixture::new().await;
    for versioned in [false, true] {
        let signed = fixture.transaction(versioned);
        let mut unsigned = signed.clone();
        unsigned.signatures.fill(Default::default());
        for (tx, method, options) in [
            (
                &unsigned,
                "sendTransaction",
                json!({"encoding":"base64","sigVerify":false}),
            ),
            (
                &unsigned,
                "simulateTransaction",
                json!({"encoding":"base64"}),
            ),
            (
                &unsigned,
                "simulateTransaction",
                json!({"encoding":"base64","sigVerify":true}),
            ),
            (
                &signed,
                "simulateTransaction",
                json!({"encoding":"base64","sigVerify":false}),
            ),
            (
                &signed,
                "simulateTransaction",
                json!({"encoding":"base64","sigVerify":true,"replaceRecentBlockhash":true}),
            ),
            (
                &unsigned,
                "simulateTransaction",
                json!({"encoding":"base64","sigVerify":false,"minContextSlot":-1}),
            ),
            (
                &unsigned,
                "simulateTransaction",
                json!({"encoding":"base64","sigVerify":false,"accounts":{"encoding":"base64","addresses":[]}}),
            ),
        ] {
            assert!(
                fixture
                    .request(&fixture.body(tx, method, options), true)
                    .await
                    .0
                    .is_client_error()
            );
        }
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
}
