mod common;
use axum::{Json, Router, routing::post};
use fossa_indexer::{App, one_shot, usage};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn key(n: u8) -> String {
    Pubkey::new_from_array([n; 32]).to_string()
}
fn fixture(list: u8, claim: bool) -> Value {
    let keys = vec![
        key(2),
        key(20),
        key(40),
        key(41),
        key(22),
        key(21),
        key(30),
        key(42),
        key(list),
        key(1),
        key(12),
        key(31),
        key(32),
        key(3),
        key(33),
        fossa_indexer::protocol::TOKEN.into(),
    ];
    let mut data = vec![0; if claim { 13 } else { 108 }];
    data[0] = if claim { 7 } else { 6 };
    if claim {
        for at in [1, 5, 9] {
            data[at..at + 4].copy_from_slice(&10u32.to_le_bytes());
        }
    }
    let mut transfer = vec![12];
    transfer.extend_from_slice(&9_007_199_254_740_993u64.to_le_bytes());
    transfer.push(6);
    json!({"slot":50,"blockTime":100,"meta":{"err":null,"innerInstructions":if claim {json!([{"index":0,"instructions":[{"programIdIndex":15,"accounts":[11,13,12,14],"data":bs58::encode(transfer).into_string()}]}])}else{json!([])}},"transaction":{"message":{"accountKeys":keys,"instructions":[{"programIdIndex":9,"accounts":if claim{vec![0,1,3,4,6,10,11,12,13,14,15]}else{(0..9).collect()},"data":bs58::encode(data).into_string()}]}}})
}
async fn save(app: &App, tx: Value, signature: &str, order: u32) {
    let events = one_shot::decode(&tx, &key(1), &key(2), &key(3)).unwrap();
    assert_eq!(events.len(), 1);
    let signature = signature.to_owned();
    app.db
        .call(move |conn| {
            let transaction = conn.transaction()?;
            one_shot::store(&transaction, &signature, 50, order, 100, &events)?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .unwrap();
}
#[tokio::test]
async fn one_shot_recreation_uses_block_order_exact_transfer_and_replay_safe_totals() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    // reverse arrival would otherwise attribute the later claim to the first reservation.
    save(&app, fixture(5, true), "claim-b", 4).await;
    save(&app, fixture(4, false), "book-a", 1).await;
    one_shot::resolve(&app).await.unwrap();
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(4)))
            .await
            .unwrap()["requests"],
        0
    );
    save(&app, fixture(4, true), "claim-a", 2).await;
    save(&app, fixture(5, false), "book-b", 3).await;
    app.db.call(|conn|{conn.execute_batch("INSERT INTO settings VALUES('history_exhausted','true'); INSERT INTO settings VALUES('receipts_checked_at','1'); INSERT INTO settings VALUES('recent_before','pending');")?;Ok(())}).await.unwrap();
    one_shot::resolve(&app).await.unwrap();
    assert!(!usage::rankings(&app).await.unwrap().0);
    app.db
        .call(|conn| {
            conn.execute("DELETE FROM settings WHERE key='recent_before'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    one_shot::resolve(&app).await.unwrap();
    for list in [4, 5] {
        let stats = usage::stats(&app, format!("list:{}", key(list)))
            .await
            .unwrap();
        assert_eq!(stats["requests"], 1);
        assert_eq!(stats["tokens"], 30);
        assert_eq!(stats["spentAtomic"], "9007199254740993");
    }
    assert!(usage::rankings(&app).await.unwrap().0);
    save(&app, fixture(4, true), "claim-a", 2).await;
    one_shot::resolve(&app).await.unwrap();
    drop(app);
    let app = App::new(config).await.unwrap();
    one_shot::resolve(&app).await.unwrap();
    assert_eq!(
        usage::stats(&app, format!("provider:{}", key(12)))
            .await
            .unwrap()["requests"],
        2
    );
    let mut zero = fixture(5, true);
    zero["meta"]["innerInstructions"] = json!([]);
    save(&app, fixture(5, false), "book-zero", 5).await;
    save(&app, zero, "zero", 6).await;
    one_shot::resolve(&app).await.unwrap();
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(5)))
            .await
            .unwrap()["requests"],
        2
    );
    assert_eq!(usage::activity(&app, key(12)).await.unwrap().len(), 3);
    let mut legacy = fixture(4, false);
    legacy["transaction"]["message"]["instructions"][0]["accounts"]
        .as_array_mut()
        .unwrap()
        .pop();
    save(&app, legacy, "legacy-barrier", 7).await;
    save(&app, fixture(4, true), "legacy-claim", 8).await;
    one_shot::resolve(&app).await.unwrap();
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(5)))
            .await
            .unwrap()["requests"],
        2
    );
    assert_eq!(
        usage::stats(&app, format!("provider:{}", key(12)))
            .await
            .unwrap()["requests"],
        4
    );
    let mut explicit_zero = fixture(5, false);
    let mut data = bs58::decode(
        explicit_zero["transaction"]["message"]["instructions"][0]["data"]
            .as_str()
            .unwrap(),
    )
    .into_vec()
    .unwrap();
    data.extend_from_slice(&0u64.to_le_bytes());
    explicit_zero["transaction"]["message"]["instructions"][0]["data"] =
        bs58::encode(data).into_string().into();
    save(&app, explicit_zero, "explicit-zero-book", 9).await;
    save(&app, fixture(5, true), "explicit-zero-claim", 10).await;
    one_shot::resolve(&app).await.unwrap();
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(5)))
            .await
            .unwrap()["requests"],
        3
    );
    let mut missing = fixture(4, true);
    missing["meta"]["innerInstructions"] = Value::Null;
    assert!(one_shot::decode(&missing, &key(1), &key(2), &key(3)).is_err());
    let mut failed = fixture(4, true);
    failed["meta"]["err"] = json!("failed");
    assert!(
        one_shot::decode(&failed, &key(1), &key(2), &key(3))
            .unwrap()
            .is_empty()
    );
    assert!(
        one_shot::decode(&fixture(4, true), &key(1), &key(99), &key(3))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn finalized_block_signature_lookup_is_bounded_cached_and_required_for_identity() {
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let signatures = vec![
        bs58::encode([1; 64]).into_string(),
        bs58::encode([2; 64]).into_string(),
    ];
    let body = signatures.clone();
    let router = Router::new().route(
        "/",
        post(move |Json(input): Json<Value>| {
            let seen = seen.clone();
            let body = body.clone();
            async move {
                seen.fetch_add(1, Ordering::Relaxed);
                assert_eq!(input["method"], "getBlock");
                assert_eq!(input["params"][1]["transactionDetails"], "signatures");
                assert_eq!(input["params"][1]["commitment"], "finalized");
                Json(json!({"jsonrpc":"2.0","id":input["id"],"result":{"signatures":body}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut config = common::config(dir.path());
    config.rpc_url = format!("http://{address}");
    let app = App::new(config).await.unwrap();
    let mut blocks = one_shot::Blocks::new();
    assert_eq!(
        one_shot::order(&app, 50, &signatures[1], &mut blocks)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        one_shot::order(&app, 50, &signatures[0], &mut blocks)
            .await
            .unwrap(),
        0
    );
    assert_eq!(requests.load(Ordering::Relaxed), 1);
    assert!(
        one_shot::order(&app, 50, "missing", &mut blocks)
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn one_shot_instruction_order_keeps_recreated_bookings_in_one_transaction_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(common::config(dir.path())).await.unwrap();
    let mut tx = fixture(4, false);
    let claim = fixture(4, true);
    let book_a = tx["transaction"]["message"]["instructions"][0].clone();
    let claim_a = claim["transaction"]["message"]["instructions"][0].clone();
    let mut book_b = book_a.clone();
    book_b["accounts"][8] = 16.into();
    tx["transaction"]["message"]["accountKeys"]
        .as_array_mut()
        .unwrap()
        .push(key(5).into());
    tx["transaction"]["message"]["instructions"] = json!([book_a, claim_a, book_b, claim_a]);
    let mut first = claim["meta"]["innerInstructions"][0].clone();
    first["index"] = 1.into();
    let mut second = first.clone();
    second["index"] = 3.into();
    tx["meta"]["innerInstructions"] = json!([first, second]);
    let events = one_shot::decode(&tx, &key(1), &key(2), &key(3)).unwrap();
    assert_eq!(events.len(), 4);
    app.db.call(move|conn|{let tx=conn.transaction()?;for _ in 0..2 {one_shot::store(&tx,"batch",50,0,100,&events)?;}tx.execute_batch("INSERT INTO settings VALUES('history_exhausted','true'); INSERT INTO settings VALUES('receipts_checked_at','1');")?;tx.commit()?;Ok(())}).await.unwrap();
    for _ in 0..2 {
        one_shot::resolve(&app).await.unwrap();
    }
    for list in [4, 5] {
        assert_eq!(
            usage::stats(&app, format!("list:{}", key(list)))
                .await
                .unwrap()["requests"],
            1
        );
    }
}

#[tokio::test]
async fn native_one_shot_usage_includes_pinned_fee_transfers() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(common::config(dir.path())).await.unwrap();
    let mut reservation = fixture(4, false);
    let mut data = vec![0; 116];
    data[..8].copy_from_slice(&Sha256::digest(b"global:reserve_booking")[..8]);
    let instruction = &mut reservation["transaction"]["message"]["instructions"][0];
    instruction["accounts"]
        .as_array_mut()
        .unwrap()
        .push(json!(9));
    instruction["data"] = bs58::encode(data).into_string().into();
    save(&app, reservation, "native-reserve", 1).await;
    let mut claim = fixture(4, true);
    let mut data = vec![0; 21];
    data[..8].copy_from_slice(&Sha256::digest(b"global:claim_booking")[..8]);
    for offset in [8, 12, 16] {
        data[offset..offset + 4].copy_from_slice(&10u32.to_le_bytes());
    }
    let instruction = &mut claim["transaction"]["message"]["instructions"][0];
    instruction["accounts"]
        .as_array_mut()
        .unwrap()
        .extend([json!(12), json!(8)]);
    instruction["data"] = bs58::encode(data).into_string().into();
    let mut fee = vec![12];
    fee.extend_from_slice(&100u64.to_le_bytes());
    fee.push(6);
    claim["meta"]["innerInstructions"][0]["instructions"].as_array_mut().unwrap().push(json!({"programIdIndex":15,"accounts":[11,13,8,14],"data":bs58::encode(fee).into_string()}));
    save(&app, claim, "native-claim", 2).await;
    app.db.call(|conn| { conn.execute_batch("INSERT INTO settings VALUES('history_exhausted','true'); INSERT INTO settings VALUES('receipts_checked_at','1');")?; Ok(()) }).await.unwrap();
    one_shot::resolve(&app).await.unwrap();
    let stats = usage::stats(&app, format!("list:{}", key(4)))
        .await
        .unwrap();
    assert_eq!(stats["requests"], 1);
    assert_eq!(stats["tokens"], 30);
    assert_eq!(stats["spentAtomic"], "9007199254741093");
}
