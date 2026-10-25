mod common;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fossa_indexer::{
    App, api,
    index::Record,
    metadata::{self, Document, schema},
    protocol::{self, Kind},
    receipts::{BookingList, Receipt},
    usage,
};
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use std::time::Instant;
use tower::ServiceExt;

fn key(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}
fn record(app: &App, data: Vec<u8>) -> Record {
    Record {
        slot: 10,
        version: 0,
        owner: app.config.program.to_string(),
        lamports: 1,
        data: data.into(),
    }
}
fn list(app: &App, provider: u8) -> Vec<u8> {
    let mut d = vec![0; 80 + 64];
    d[..8].copy_from_slice(b"LIST0V30");
    d[8..40].copy_from_slice(app.config.market.as_ref());
    d[40..72].fill(9);
    d[72..76].copy_from_slice(&1u32.to_le_bytes());
    d[76..80].copy_from_slice(&1u32.to_le_bytes());
    d[80..112].fill(provider);
    d[112..144].fill(provider);
    d
}
async fn profile(app: &App, id: u8, name: &str) -> (String, Record) {
    let raw = serde_json::to_vec(
        &json!({"name":name,"accentColor":"#8877AA","icon":"https://example.com/icon.svg"}),
    )
    .unwrap();
    let value = schema::parse(schema::Kind::List, &raw).unwrap();
    let hash = Sha256::digest(&raw);
    let (address, bump) =
        Pubkey::find_program_address(&[b"list_metadata", key(id).as_ref()], &app.config.program);
    let mut d = vec![0; 396];
    d[..8].copy_from_slice(b"LMETAV30");
    d[8..40].copy_from_slice(app.config.market.as_ref());
    d[40..72].fill(id);
    d[72..104].fill(9);
    d[104..136].copy_from_slice(&hash);
    d[138] = bump;
    let uri = b"https://example.com/list.json";
    d[136..138].copy_from_slice(&(uri.len() as u16).to_le_bytes());
    d[140..140 + uri.len()].copy_from_slice(uri);
    app.index.state.write().await.documents.insert(
        metadata::key(&d).unwrap(),
        Document {
            raw: raw.into(),
            value,
        },
    );
    (address.to_string(), record(app, d))
}
async fn setup(app: &App) {
    let mut market = vec![0; 72];
    market[..8].copy_from_slice(b"FOSSAV20");
    market[40..72].copy_from_slice(app.config.mint.as_ref());
    let mut records = vec![
        (app.config.market.to_string(), record(app, market)),
        (key(4).to_string(), record(app, list(app, 11))),
        (key(5).to_string(), record(app, list(app, 12))),
    ];
    for (id, name) in [(4, "Alpha"), (5, "Beta")] {
        records.push(profile(app, id, name).await);
    }
    for n in [11, 12] {
        let raw=serde_json::to_vec(&json!({"name":format!("Provider {n}"),"endpoint":"https://example.com/v1/chat/completions"})).unwrap();
        let value = schema::parse(schema::Kind::Provider, &raw).unwrap();
        let (address, bump) = Pubkey::find_program_address(
            &[b"provider", app.config.market.as_ref(), key(n).as_ref()],
            &app.config.program,
        );
        let mut d = vec![0; 364];
        d[..8].copy_from_slice(b"PROV0V20");
        d[8..40].copy_from_slice(app.config.market.as_ref());
        d[40..72].fill(n);
        d[72..104].copy_from_slice(&Sha256::digest(&raw));
        d[107] = bump;
        d[104..106].copy_from_slice(&1u16.to_le_bytes());
        d[108] = b'x';
        app.index.state.write().await.documents.insert(
            metadata::key(&d).unwrap(),
            Document {
                raw: raw.into(),
                value,
            },
        );
        records.push((address.to_string(), record(app, d)));
    }
    app.index
        .apply(&app.db, &app.config, records, Some(10))
        .await
        .unwrap();
    app.index.connected(true).await;
    let mut state = app.index.state.write().await;
    state.reconciled = Some(Instant::now());
    state.activity = Some(Instant::now());
}
fn receipt(index: u64) -> Receipt {
    Receipt {
        version: 2,
        bucket: key(30).to_string(),
        epoch: 0,
        index,
        receipt: {
            let mut raw = vec![0; 120];
            raw[32..64].fill(22);
            STANDARD.encode(raw)
        },
        model: key(20).to_string(),
        provider: key(12).to_string(),
        owner: key(21).to_string(),
        booking: key(30).to_string(),
        generation: index.to_string(),
        charge: u64::MAX.to_string(),
        deadline: 0,
        time: 0,
        tokens: 33,
        signature: format!("sig{index}"),
        slot: 50,
    }
}
fn booking(receipt: &Receipt, list: u8) -> BookingList {
    BookingList {
        booking: receipt.booking.clone(),
        generation: receipt.generation.clone(),
        list: key(list).to_string(),
        lane: receipt.lane().unwrap(),
        owner: receipt.owner.clone(),
        model: receipt.model.clone(),
        slot: 49,
    }
}
async fn store(app: &App, bookings: Vec<BookingList>, receipts: Vec<Receipt>) {
    app.db
        .call(move |conn| {
            let tx = conn.transaction()?;
            usage::store(&tx, &bookings, &receipts)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
}
async fn get(app: &App, path: &str) -> (u16, Value) {
    let response = api::router(app.clone())
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn list_usage_is_exact_idempotent_order_independent_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    let r1 = receipt(1);
    let b1 = booking(&r1, 5);
    store(&app, vec![], vec![r1.clone()]).await;
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(5)))
            .await
            .unwrap()["requests"],
        0
    );
    store(&app, vec![b1.clone()], vec![]).await;
    store(&app, vec![b1.clone()], vec![r1.clone()]).await;
    // later membership and conflicting replay cannot move the original selection.
    store(&app, vec![booking(&r1, 4)], vec![r1.clone()]).await;
    // a recreated booking with the same generation and owner but a new lane is distinct.
    let mut recreated = r1.clone();
    let mut raw = STANDARD.decode(&recreated.receipt).unwrap();
    raw[32..64].fill(23);
    recreated.receipt = STANDARD.encode(raw);
    recreated.signature = "recreated".into();
    store(&app, vec![booking(&recreated, 4)], vec![recreated]).await;
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(4)))
            .await
            .unwrap()["requests"],
        1
    );
    let r2 = receipt(2);
    store(&app, vec![booking(&r2, 5)], vec![r2.clone()]).await;
    let stats = usage::stats(&app, format!("list:{}", key(5)))
        .await
        .unwrap();
    assert_eq!(stats["requests"], 2);
    assert_eq!(stats["tokens"], 66);
    assert_eq!(stats["spentAtomic"], (u64::MAX as u128 * 2).to_string());
    assert_eq!(stats["topProvider"], key(12).to_string());
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(4)))
            .await
            .unwrap()["requests"],
        1
    );
    // wrong owner/model, later booking slots, missing generation stay unassigned.
    for (index, field) in [(3, 0), (4, 1), (5, 2), (6, 3)] {
        let r = receipt(index);
        let mut b = booking(&r, 4);
        match field {
            0 => b.owner = key(90).to_string(),
            1 => b.model = key(90).to_string(),
            2 => b.slot = 51,
            _ => b.generation = "999".into(),
        };
        store(&app, vec![b], vec![r]).await;
    }
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(4)))
            .await
            .unwrap()["requests"],
        1
    );
    // successful zero-charge receipts still count.
    let mut r = receipt(7);
    r.charge = "0".into();
    store(&app, vec![booking(&r, 5)], vec![r]).await;
    let provider = usage::stats(&app, format!("provider:{}", key(12)))
        .await
        .unwrap();
    assert_eq!(provider["requests"], 8);
    drop(app);
    let app = App::new(config).await.unwrap();
    store(&app, vec![b1], vec![r1]).await;
    assert_eq!(
        usage::stats(&app, format!("list:{}", key(5)))
            .await
            .unwrap()["requests"],
        3
    );
    assert!(!usage::rankings(&app).await.unwrap().0);
    app.db.call(|conn|{conn.execute_batch("INSERT INTO settings VALUES('history_exhausted','true'); INSERT INTO settings VALUES('receipts_checked_at','1');")?;Ok(())}).await.unwrap();
    assert!(usage::rankings(&app).await.unwrap().0);
}

#[tokio::test]
async fn discovery_profiles_membership_search_and_invalid_metadata_are_bound_to_chain() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(common::config(dir.path())).await.unwrap();
    setup(&app).await;
    let r = receipt(1);
    store(&app, vec![booking(&r, 5)], vec![r]).await;
    let (status, lists) = get(&app, "/v1/market/lists").await;
    assert_eq!(status, 200);
    assert_eq!(lists["total"], 2);
    assert_eq!(lists["lists"][0]["name"], "Beta");
    assert_eq!(lists["lists"][0]["rank"], 1);
    assert_eq!(lists["lists"][0]["providers"][0]["name"], "Provider 12");
    assert_eq!(lists["lists"][0]["providerCount"], 1);
    assert!(
        lists["lists"][0]["icon"]
            .as_str()
            .unwrap()
            .contains(&format!("/v1/market/lists/{}/icon?version=", key(5)))
    );
    let (_, filtered) = get(&app, "/v1/market/lists?q=alpha").await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["lists"][0]["rank"], 2);
    let (status, profile) = get(&app, &format!("/v1/market/lists/{}/profile", key(5))).await;
    assert_eq!(status, 200);
    assert_eq!(profile["requests"], 1);
    assert_eq!(profile["topProvider"], "Provider 12");
    assert!(profile.get("activity").is_none());
    assert_eq!(profile["providers"].as_array().unwrap().len(), 1);
    let (status, provider) = get(&app, &format!("/v1/market/providers/{}/profile", key(12))).await;
    assert_eq!(status, 200);
    assert_eq!(provider["provider"]["name"], "Provider 12");
    assert_eq!(provider["lists"][0]["name"], "Beta");
    let (status, legacy) = get(&app, &format!("/v1/market/providers/{}", key(12))).await;
    assert_eq!(status, 200);
    assert!(legacy["receipts"].is_object());
    assert_eq!(legacy["lists"][0]["name"], "Beta");
    assert_eq!(
        get(&app, &format!("/v1/market/lists/{}/profile", key(99)))
            .await
            .0,
        404
    );
    assert_eq!(get(&app, "/v1/market/lists/not-a-key/profile").await.0, 400);
    assert_eq!(
        get(&app, &format!("/v1/market/lists?q={}", "x".repeat(257)))
            .await
            .0,
        400
    );
    let address =
        Pubkey::find_program_address(&[b"list_metadata", key(5).as_ref()], &app.config.program)
            .0
            .to_string();
    let original = app.index.state.read().await.accounts[&address]
        .data
        .to_vec();
    assert_eq!(
        protocol::validate(&address, &original, &app.config),
        Some(Kind::ListMetadata)
    );
    for at in [0, 8, 40, 138, 139, 140, 395] {
        let mut d = original.clone();
        d[at] ^= 255;
        assert!(protocol::validate(&address, &d, &app.config).is_none());
    }
    for length in [0u16, 257, u16::MAX] {
        let mut d = original.clone();
        d[136..138].copy_from_slice(&length.to_le_bytes());
        assert!(protocol::validate(&address, &d, &app.config).is_none());
    }
    let mut wrong_authority = original;
    wrong_authority[72..104].fill(90);
    let mut r = record(&app, wrong_authority);
    r.slot = 11;
    app.index
        .apply(&app.db, &app.config, vec![(address, r)], None)
        .await
        .unwrap();
    let (_, profile) = get(&app, &format!("/v1/market/lists/{}/profile", key(5))).await;
    assert_ne!(profile["list"]["name"], "Beta");
    assert!(profile["list"]["icon"].is_null());
    assert_eq!(
        get(&app, &format!("/v1/market/lists/{}/icon", key(5)))
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn historical_signature_reprocessing_resumes_once_without_deleting_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    app.db
        .call(|conn| {
            conn.execute("INSERT INTO signatures VALUES('old',1,1,4,999)", [])?;
            conn.execute("DELETE FROM settings WHERE key='list_usage_version'", [])?;
            conn.execute(
                "INSERT INTO receipts VALUES('old','owner','provider',1,?1)",
                params!["{}"],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(app);
    let app = App::new(config.clone()).await.unwrap();
    app.db
        .call(|conn| {
            let state: (u8, u8, u64) = conn.query_row(
                "SELECT done,attempts,next_attempt FROM signatures WHERE signature='old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(state, (0, 0, 0));
            assert_eq!(
                conn.query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, u64>(0))?,
                1
            );
            conn.execute("UPDATE signatures SET done=1", [])?;
            Ok(())
        })
        .await
        .unwrap();
    drop(app);
    let app = App::new(config).await.unwrap();
    assert_eq!(
        app.db
            .call(|conn| Ok(conn.query_row(
                "SELECT done FROM signatures WHERE signature='old'",
                [],
                |r| r.get::<_, u8>(0)
            )?))
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn discovery_bounds_rows_and_previews_while_profiles_keep_all_members() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(common::config(dir.path())).await.unwrap();
    setup(&app).await;
    let cached = app.index.catalog(&app.config).await;
    let mut capacity = vec![0; 80];
    capacity[..8].copy_from_slice(b"CAPACV20");
    capacity[40..44].copy_from_slice(&1u32.to_le_bytes());
    let mut update = record(&app, capacity);
    update.slot = 11;
    app.index
        .apply(
            &app.db,
            &app.config,
            vec![(key(60).to_string(), update)],
            None,
        )
        .await
        .unwrap();
    let after = app.index.catalog(&app.config).await;
    assert!(std::sync::Arc::ptr_eq(&cached.directory, &after.directory));
    let mut data = vec![0; 80 + 4096 * 64];
    data[..80].copy_from_slice(&list(&app, 11)[..80]);
    data[72..76].copy_from_slice(&4096u32.to_le_bytes());
    data[76..80].copy_from_slice(&4096u32.to_le_bytes());
    for i in 0..4096 {
        data[80 + i * 32] = 70;
        data[80 + i * 32 + 28..80 + i * 32 + 32].copy_from_slice(&(i as u32 + 1).to_be_bytes());
    }
    fn tree(data: &mut [u8], node: usize, at: &mut usize) {
        if node >= 4096 {
            return;
        }
        tree(data, node * 2 + 1, at);
        for word in 0..4 {
            let start = 80 + *at * 32 + word * 8;
            let value = u64::from_be_bytes(data[start..start + 8].try_into().unwrap());
            let end = 80 + 4096 * 32 + node * 32 + word * 8;
            data[end..end + 8].copy_from_slice(&value.to_le_bytes());
        }
        *at += 1;
        tree(data, node * 2 + 2, at);
    }
    tree(&mut data, 0, &mut 0);
    let mut large = record(&app, data);
    large.slot = 12;
    let mut updates = vec![(key(4).to_string(), large)];
    for i in 0..150u32 {
        let mut address = [80; 32];
        address[28..].copy_from_slice(&i.to_be_bytes());
        let mut r = record(&app, list(&app, 11));
        r.slot = 12;
        updates.push((Pubkey::new_from_array(address).to_string(), r));
    }
    app.index
        .apply(&app.db, &app.config, updates, None)
        .await
        .unwrap();
    let (_, directory) = get(&app, "/v1/market/lists").await;
    assert_eq!(directory["total"], 152);
    assert_eq!(directory["lists"].as_array().unwrap().len(), 100);
    assert!(
        directory["lists"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["providers"].as_array().unwrap().len() <= 12)
    );
    let (status, profile) = get(&app, &format!("/v1/market/lists/{}/profile", key(4))).await;
    assert_eq!(status, 200);
    assert_eq!(profile["providers"].as_array().unwrap().len(), 4096);
    assert_eq!(profile["list"]["providers"].as_array().unwrap().len(), 12);
    assert_eq!(profile["list"]["providerCount"], 4096);
    assert!(!std::sync::Arc::ptr_eq(
        &after.directory,
        &app.index.catalog(&app.config).await.directory
    ));
}
