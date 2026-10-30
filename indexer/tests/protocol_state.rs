mod common;
use base64::{Engine, engine::general_purpose::STANDARD};
use fossa_indexer::{
    App,
    index::Record,
    metadata::{Document, schema},
    protocol::{self, Kind},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use std::sync::Arc;

fn record(
    config: &fossa_indexer::config::Config,
    slot: u64,
    version: u64,
    data: Vec<u8>,
) -> Record {
    Record {
        slot,
        version,
        owner: config.program.to_string(),
        lamports: 1,
        data: data.into(),
    }
}
fn market(config: &fossa_indexer::config::Config) -> Vec<u8> {
    let mut data = vec![0; 72];
    data[..8].copy_from_slice(b"FOSSAV20");
    data[40..72].copy_from_slice(config.mint.as_ref());
    data
}
fn booking(config: &fossa_indexer::config::Config, status: u8) -> Vec<u8> {
    let mut data = vec![0; 368];
    data[..8].copy_from_slice(b"BOOK0V20");
    data[72..104].copy_from_slice(config.market.as_ref());
    data[148] = status;
    data
}

fn group(config: &fossa_indexer::config::Config) -> (Pubkey, Vec<u8>) {
    let id = [42; 32];
    let (address, bump) = Pubkey::find_program_address(
        &[b"model_group", config.market.as_ref(), &id],
        &config.program,
    );
    let mint =
        Pubkey::find_program_address(&[b"reserve_mint", address.as_ref()], &config.program).0;
    let mut data = vec![0; 144];
    data[..8].copy_from_slice(&Sha256::digest(b"account:ModelGroup")[..8]);
    data[8..40].copy_from_slice(config.market.as_ref());
    data[40..72].copy_from_slice(&id);
    data[72..104].fill(43);
    data[104..136].copy_from_slice(mint.as_ref());
    data[136] = 1;
    data[137] = bump;
    (address, data)
}

#[tokio::test]
async fn native_groups_and_market_policy_survive_restart_and_invalidate_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    let (address, mut data) = group(&config);
    assert_eq!(
        protocol::validate(&address.to_string(), &data, &config),
        Some(Kind::ModelGroup)
    );
    for offset in [8, 40, 104, 137, 138] {
        let mut bad = data.clone();
        bad[offset] ^= 1;
        assert!(protocol::validate(&address.to_string(), &bad, &config).is_none());
    }
    let mut bad = data.clone();
    bad[72..104].fill(0);
    assert!(protocol::validate(&address.to_string(), &bad, &config).is_none());
    bad = data.clone();
    bad[136] = 2;
    assert!(protocol::kind(&bad).is_none());
    let mut market = market(&config);
    market.resize(248, 0);
    market[..8].copy_from_slice(&protocol::MARKET_DISCRIMINATOR);
    market[8..40].fill(44);
    market[72..104].fill(45);
    market[168..176].copy_from_slice(&u64::MAX.to_le_bytes());
    app.index
        .apply(
            &app.db,
            &config,
            vec![
                (config.market.to_string(), record(&config, 10, 0, market)),
                (address.to_string(), record(&config, 10, 0, data.clone())),
            ],
            Some(10),
        )
        .await
        .unwrap();
    let catalog = app.index.catalog(&config).await;
    assert_eq!(
        catalog.value["marketConfig"]["creationNetPrice"],
        u64::MAX.to_string()
    );
    assert_eq!(catalog.value["marketConfig"]["creationEnabled"], false);
    assert!(catalog.value["marketConfig"]["creationMint"].is_null());
    assert!(catalog.value["marketConfig"]["dexProgram"].is_null());
    assert_eq!(
        catalog.value["modelGroups"][0]["address"],
        address.to_string()
    );
    assert_eq!(catalog.value["modelGroups"][0]["routeToReserve"], true);
    data[136] = 0;
    app.index
        .apply(
            &app.db,
            &config,
            vec![(address.to_string(), record(&config, 11, 1, data))],
            None,
        )
        .await
        .unwrap();
    let changed = app.index.catalog(&config).await;
    assert!(!Arc::ptr_eq(&catalog, &changed));
    assert_eq!(changed.value["modelGroups"][0]["routeToReserve"], false);
    drop(app);
    let app = App::new(config.clone()).await.unwrap();
    assert_eq!(
        app.index.catalog(&config).await.value["modelGroups"],
        changed.value["modelGroups"]
    );
    app.index
        .apply(
            &app.db,
            &config,
            vec![(address.to_string(), Record::closed(12, 2))],
            None,
        )
        .await
        .unwrap();
    assert!(
        app.index.catalog(&config).await.value["modelGroups"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn booking_ownership_index_tracks_replacement_close_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    let owner = Pubkey::new_from_array([21; 32]).to_string();
    let lane = Pubkey::new_from_array([22; 32]).to_string();
    let other = Pubkey::new_from_array([23; 32]).to_string();
    let id = Pubkey::new_from_array([24; 32]).to_string();
    let mut data = booking(&config, 5);
    data[8..40].fill(21);
    data[192..224].fill(22);
    let session = fossa_indexer::auth::Session {
        wallet: owner.clone(),
        lane: lane.clone(),
        family: "test".into(),
        expires: u64::MAX,
    };
    app.index
        .apply(
            &app.db,
            &config,
            vec![
                (
                    config.market.to_string(),
                    record(&config, 10, 0, market(&config)),
                ),
                (id.clone(), record(&config, 10, 0, data.clone())),
            ],
            Some(10),
        )
        .await
        .unwrap();
    assert_eq!(
        fossa_indexer::api::wallet::booking_pool(&app, &session)
            .await
            .len(),
        1
    );
    data[8..40].fill(23);
    app.index
        .apply(
            &app.db,
            &config,
            vec![(id.clone(), record(&config, 11, 1, data))],
            None,
        )
        .await
        .unwrap();
    assert!(
        fossa_indexer::api::wallet::booking_pool(&app, &session)
            .await
            .is_empty()
    );
    let session = fossa_indexer::auth::Session {
        wallet: other,
        ..session
    };
    assert_eq!(
        fossa_indexer::api::wallet::booking_pool(&app, &session)
            .await
            .len(),
        1
    );
    drop(app);
    let app = App::new(config.clone()).await.unwrap();
    assert_eq!(
        fossa_indexer::api::wallet::booking_pool(&app, &session)
            .await
            .len(),
        1
    );
    app.index
        .apply(&app.db, &config, vec![(id, Record::closed(12, 2))], None)
        .await
        .unwrap();
    assert!(
        fossa_indexer::api::wallet::booking_pool(&app, &session)
            .await
            .is_empty()
    );
    assert!(app.index.state.read().await.bookings.is_empty());
}

#[tokio::test]
async fn metadata_restore_bounds_total_bytes_without_deleting_persisted_documents() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config).await.unwrap();
    let raw =
        br#"{"name":"Example","endpoint":"https://example.com/v1/chat/completions"}"#.to_vec();
    let second =
        br#"{"name":"Second","endpoint":"https://example.com/v1/chat/completions"}"#.to_vec();
    let budget = raw.len().max(second.len());
    app.db
        .call(move |conn| {
            for raw in [raw, second] {
                conn.execute(
                    "INSERT INTO documents VALUES(?1,'provider',?2)",
                    rusqlite::params![hex::encode(Sha256::digest(&raw)), raw],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let restored = fossa_indexer::metadata::restore(&app.db, 100, budget)
        .await
        .unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored.values().map(|doc| doc.raw.len()).sum::<usize>() <= budget);
    assert_eq!(
        app.db
            .call(|conn| Ok(conn
                .query_row("SELECT count(*) FROM documents", [], |row| row
                    .get::<_, usize>(0))?))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        fossa_indexer::metadata::restore(&app.db, 100, budget * 2)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn state_rejects_replay_reconciles_deletes_recovers_and_replaces_at_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = common::config(dir.path());
    config.max_accounts = 2;
    let app = App::new(config.clone()).await.unwrap();
    let id = Pubkey::new_from_array([10; 32]).to_string();
    let other = Pubkey::new_from_array([11; 32]).to_string();
    app.index
        .apply(
            &app.db,
            &config,
            vec![
                (
                    config.market.to_string(),
                    record(&config, 10, 0, market(&config)),
                ),
                (id.clone(), record(&config, 10, 0, booking(&config, 0))),
            ],
            Some(10),
        )
        .await
        .unwrap();
    assert!(!app.index.ready(&config).await);
    app.index.connected(true).await;
    app.index.activity().await;
    assert!(app.index.ready(&config).await);
    for (slot, version, status) in [(11, 4, 1), (11, 3, 0), (10, 50, 0), (11, 5, 5)] {
        app.index
            .apply(
                &app.db,
                &config,
                vec![(
                    id.clone(),
                    record(&config, slot, version, booking(&config, status)),
                )],
                None,
            )
            .await
            .unwrap();
    }
    assert_eq!(app.index.state.read().await.accounts[&id].data[148], 5);
    app.index
        .apply(
            &app.db,
            &config,
            vec![(id.clone(), Record::closed(12, 6))],
            None,
        )
        .await
        .unwrap();
    app.index
        .apply(
            &app.db,
            &config,
            vec![(id.clone(), record(&config, 11, 100, booking(&config, 0)))],
            None,
        )
        .await
        .unwrap();
    assert!(app.index.state.read().await.accounts[&id].data.is_empty());
    app.index
        .apply(
            &app.db,
            &config,
            vec![
                (
                    config.market.to_string(),
                    record(&config, 13, 0, market(&config)),
                ),
                (other.clone(), record(&config, 13, 0, booking(&config, 0))),
            ],
            Some(13),
        )
        .await
        .unwrap();
    assert_eq!(app.index.state.read().await.accounts.len(), 2);
    assert!(!app.index.state.read().await.accounts.contains_key(&id));
    drop(app);
    let restored = App::new(config.clone()).await.unwrap();
    assert!(!restored.index.ready(&config).await);
    assert_eq!(restored.index.state.read().await.floor, 13);
    assert!(
        restored
            .index
            .state
            .read()
            .await
            .accounts
            .contains_key(&other)
    );
    restored
        .index
        .apply(
            &restored.db,
            &config,
            vec![(id.clone(), record(&config, 12, 500, booking(&config, 0)))],
            None,
        )
        .await
        .unwrap();
    assert!(!restored.index.state.read().await.accounts.contains_key(&id));
    let mut wrong = config.clone();
    wrong.market = Pubkey::new_from_array([99; 32]);
    assert!(App::new(wrong).await.is_err());
}

#[tokio::test]
async fn verified_catalog_requires_canonical_pdas_hash_books_and_provider_list() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    let wallet = Pubkey::new_from_array([8; 32]);
    let (provider, bump) = Pubkey::find_program_address(
        &[b"provider", config.market.as_ref(), wallet.as_ref()],
        &config.program,
    );
    let provider_doc =
        json!({"name":"Example","endpoint":"https://example.com/v1/chat/completions"});
    let model_doc = json!({"id":"example/model","name":"Example Model","modalities":{"input":["text"],"output":["text"]},"contextTokens":1000,"outputTokens":100});
    let id_hash = Sha256::digest(b"example/model");
    let (model, model_bump) = Pubkey::find_program_address(
        &[b"model", config.market.as_ref(), &id_hash],
        &config.program,
    );
    let (meta, meta_bump) =
        Pubkey::find_program_address(&[b"model_metadata", model.as_ref()], &config.program);
    let terms = Pubkey::new_from_array([10; 32]);
    let capacity = Pubkey::new_from_array([11; 32]);
    let mut list = vec![0; 208];
    list[..8].copy_from_slice(b"LIST0V30");
    list[8..40].copy_from_slice(config.market.as_ref());
    list[40..72].copy_from_slice(wallet.as_ref());
    list[72..76].copy_from_slice(&2u32.to_le_bytes());
    list[76..80].copy_from_slice(&1u32.to_le_bytes());
    list[80..112].copy_from_slice(wallet.as_ref());
    list[144..176].fill(8);
    let mut rows = vec![
        (config.market.to_string(), market(&config)),
        (config.provider_list.to_string(), list.clone()),
    ];
    let mut docs = Vec::new();
    for (address, kind, value, base, hash_at, len_at, uri_at) in [
        (
            provider,
            schema::Kind::Provider,
            provider_doc,
            364,
            72,
            104,
            108,
        ),
        (meta, schema::Kind::Model, model_doc, 396, 104, 136, 140),
    ] {
        let raw = serde_json::to_vec(&value).unwrap();
        let hash = Sha256::digest(&raw);
        let uri = "https://example.com/metadata.json";
        let mut data = vec![0; base];
        data[..8].copy_from_slice(if base == 364 {
            b"PROV0V20"
        } else {
            b"MMETAV20"
        });
        data[8..40].copy_from_slice(config.market.as_ref());
        data[40..72].copy_from_slice(if base == 364 {
            wallet.as_ref()
        } else {
            model.as_ref()
        });
        data[hash_at..hash_at + 32].copy_from_slice(&hash);
        data[len_at..len_at + 2].copy_from_slice(&(uri.len() as u16).to_le_bytes());
        data[uri_at..uri_at + uri.len()].copy_from_slice(uri.as_bytes());
        if base == 364 {
            data[106] = 255;
            data[107] = bump;
        } else {
            data[138] = meta_bump;
        }
        assert!(protocol::validate(&address.to_string(), &data, &config).is_some());
        let key = format!("{}:{}", kind.name(), hex::encode(hash));
        docs.push((
            key,
            Document {
                raw: Arc::from(raw),
                value,
            },
        ));
        rows.push((address.to_string(), data));
    }
    let mut data = vec![0; 144];
    data[..8].copy_from_slice(b"MODELV20");
    data[8..40].copy_from_slice(config.market.as_ref());
    data[40..72].copy_from_slice(&id_hash);
    data[72..104].copy_from_slice(terms.as_ref());
    data[104..136].copy_from_slice(capacity.as_ref());
    data[136..140].copy_from_slice(&2u32.to_le_bytes());
    data[140] = model_bump;
    assert_eq!(
        protocol::validate(&model.to_string(), &data, &config),
        Some(Kind::Model)
    );
    assert!(protocol::validate(&wallet.to_string(), &data, &config).is_none());
    rows.push((model.to_string(), data));
    for (address, len, magic) in [(terms, 288, b"TERMSV20"), (capacity, 112, b"CAPACV20")] {
        let mut data = vec![0; len];
        data[..8].copy_from_slice(magic);
        data[8..40].copy_from_slice(model.as_ref());
        data[40..44].copy_from_slice(&2u32.to_le_bytes());
        data[44..48].copy_from_slice(&2u32.to_le_bytes());
        if len == 288 {
            data[48..80].copy_from_slice(wallet.as_ref());
            data[144..148].copy_from_slice(&100u32.to_le_bytes());
            data[152..156].copy_from_slice(&200u32.to_le_bytes());
            data[156..158].copy_from_slice(&50u16.to_le_bytes());
            data[159] = 1;
            data[160] = 255;
            data[164..168].copy_from_slice(&5u32.to_le_bytes());
            data.copy_within(48..168, 168);
            data[168..200].fill(9);
        } else {
            data[48..56].copy_from_slice(&9_007_199_254_740_993u64.to_le_bytes());
        }
        rows.push((address.to_string(), data));
    }
    app.index
        .apply(
            &app.db,
            &config,
            rows.into_iter()
                .map(|(a, d)| (a, record(&config, 10, 0, d)))
                .collect(),
            Some(10),
        )
        .await
        .unwrap();
    {
        let mut state = app.index.state.write().await;
        state.documents.extend(docs);
    }
    app.index.connected(true).await;
    app.index.activity().await;
    let catalog = app.index.catalog(&config).await;
    assert!(catalog.ready);
    assert_eq!(catalog.value["models"][0]["id"], "example/model");
    assert_eq!(
        catalog.value["providerList"],
        config.provider_list.to_string()
    );
    assert_eq!(catalog.value["models"][0]["offerCount"], 2);
    assert_eq!(
        catalog.value["models"][0]["offers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for field in ["fundedBond", "exposure", "approved", "bondVault"] {
        assert!(catalog.value["models"][0]["offers"][0].get(field).is_none());
    }
    assert!(catalog.value["providers"][0].get("approved").is_none());
    assert_eq!(catalog.value["models"][0]["offers"][0]["available"], true);
    let unchanged = app
        .index
        .state
        .read()
        .await
        .accounts
        .iter()
        .map(|(address, record)| {
            let mut record = record.clone();
            record.slot = 11;
            (address.clone(), record)
        })
        .collect();
    app.index
        .apply(&app.db, &config, unchanged, Some(11))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&catalog, &app.index.catalog(&config).await));
    assert_eq!(app.index.state.read().await.floor, 11);
    let mut updated = app.index.state.read().await.accounts[&capacity.to_string()].clone();
    let mut bytes = updated.data.to_vec();
    bytes[64..68].copy_from_slice(&5u32.to_le_bytes());
    updated.data = bytes.into();
    updated.slot = 12;
    updated.version = 1;
    app.index
        .apply(
            &app.db,
            &config,
            vec![(capacity.to_string(), updated)],
            None,
        )
        .await
        .unwrap();
    let changed = app.index.catalog(&config).await;
    assert!(!Arc::ptr_eq(&catalog, &changed));
    assert_eq!(changed.value["models"][0]["offers"][0]["available"], false);
    assert_eq!(changed.value["models"][0]["offers"][0]["activeBookings"], 5);
    let (group_address, group_data) = group(&config);
    let mut grouped_model = app.index.state.read().await.accounts[&model.to_string()]
        .data
        .to_vec();
    grouped_model.resize(176, 0);
    grouped_model[..8].copy_from_slice(&protocol::MODEL_DISCRIMINATOR);
    grouped_model[144..176].copy_from_slice(group_address.as_ref());
    let mut capacity_data = app.index.state.read().await.accounts[&capacity.to_string()]
        .data
        .to_vec();
    capacity_data[64..68].fill(0);
    app.index
        .apply(
            &app.db,
            &config,
            vec![
                (model.to_string(), record(&config, 12, 2, grouped_model)),
                (capacity.to_string(), record(&config, 12, 2, capacity_data)),
            ],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        app.index.catalog(&config).await.value["models"][0]["offers"][0]["available"],
        false
    );
    app.index
        .apply(
            &app.db,
            &config,
            vec![(
                group_address.to_string(),
                record(&config, 12, 3, group_data),
            )],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        app.index.catalog(&config).await.value["models"][0]["offers"][0]["available"],
        true
    );
    list[76..80].fill(0);
    list[80..].fill(0);
    app.index
        .apply(
            &app.db,
            &config,
            vec![(
                config.provider_list.to_string(),
                record(&config, 13, 1, list),
            )],
            None,
        )
        .await
        .unwrap();
    let removed = app.index.catalog(&config).await;
    assert!(removed.ready);
    assert!(removed.value["providers"].as_array().unwrap().is_empty());
    assert!(removed.value["models"].as_array().unwrap().is_empty());
    app.index
        .apply(
            &app.db,
            &config,
            vec![(config.provider_list.to_string(), Record::closed(14, 2))],
            None,
        )
        .await
        .unwrap();
    let missing = app.index.catalog(&config).await;
    assert!(!missing.ready);
    assert!(missing.value["models"].as_array().unwrap().is_empty());
    assert!(Arc::ptr_eq(&missing, &app.index.catalog(&config).await));
    let profile = fossa_indexer::api::wallet::provider(
        axum::extract::State(app.clone()),
        axum::extract::Path(wallet.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(profile.0["provider"]["wallet"], wallet.to_string());
    assert_eq!(
        profile.0["provider"]["endpoint"],
        "https://example.com/v1/chat/completions"
    );
    app.index.connected(false).await;
    let catalog = app.index.catalog(&config).await;
    assert!(!Arc::ptr_eq(&catalog, &changed));
    assert!(!catalog.ready);
    assert!(catalog.value["models"].as_array().unwrap().is_empty());
    assert!(
        protocol::bytes(&json!({"data":[STANDARD.encode([0u8;10]),"base64"],"executable":true}))
            .is_none()
    );
}

#[test]
fn provider_lists_reject_malformed_foreign_unsorted_duplicate_and_zero_members() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let mut list = vec![0; 272];
    list[..8].copy_from_slice(b"LIST0V30");
    list[8..40].copy_from_slice(config.market.as_ref());
    list[72..76].copy_from_slice(&3u32.to_le_bytes());
    list[76..80].copy_from_slice(&2u32.to_le_bytes());
    list[80..112].fill(5);
    list[112..144].fill(6);
    list[176..208].fill(6);
    list[208..240].fill(5);
    assert_eq!(
        protocol::validate(&config.provider_list.to_string(), &list, &config),
        Some(Kind::ProviderList)
    );
    let mut bad = list.clone();
    bad.push(0);
    assert!(protocol::kind(&bad).is_none());
    for (start, end, value) in [
        (8, 40, 9),
        (72, 76, 0),
        (76, 80, 255),
        (112, 144, 5),
        (112, 144, 4),
        (80, 112, 0),
        (144, 145, 1),
        (176, 177, 1),
        (207, 208, 5),
        (208, 209, 6),
        (240, 241, 1),
    ] {
        let mut bad = list.clone();
        bad[start..end].fill(value);
        assert!(protocol::validate(&config.provider_list.to_string(), &bad, &config).is_none());
    }
    list[76..80].copy_from_slice(&3u32.to_le_bytes());
    for (index, value) in [255u32, 256, 0x80000000].into_iter().enumerate() {
        list[80 + index * 32..108 + index * 32].fill(1);
        list[108 + index * 32..112 + index * 32].copy_from_slice(&value.to_be_bytes());
    }
    for (node, index) in [(0, 1), (1, 0), (2, 2)] {
        for word in 0..4 {
            let key = 80 + index * 32 + word * 8;
            let target = 176 + node * 32 + word * 8;
            let value = u64::from_be_bytes(list[key..key + 8].try_into().unwrap());
            list[target..target + 8].copy_from_slice(&value.to_le_bytes());
        }
    }
    assert_eq!(protocol::kind(&list), Some(Kind::ProviderList));
    list[200..208].reverse();
    assert!(protocol::kind(&list).is_none());
}

#[test]
fn native_accounts_preserve_bindings_and_validate_list_fees_without_legacy_cache() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let mut native_market = market(&config);
    native_market.resize(248, 0);
    native_market[..8].copy_from_slice(&Sha256::digest(b"account:Market")[..8]);
    assert_eq!(
        protocol::validate(&config.market.to_string(), &native_market, &config),
        Some(Kind::Market)
    );
    let lane = Pubkey::new_from_array([25; 32]);
    let mut user = vec![0; 128];
    user[..8].copy_from_slice(&Sha256::digest(b"account:User")[..8]);
    user[40..72].copy_from_slice(config.market.as_ref());
    user[80..112].copy_from_slice(protocol::vault(&lane, &config).as_ref());
    user[112..120].copy_from_slice(&u64::MAX.to_le_bytes());
    user[120] = Pubkey::find_program_address(&[b"vault", lane.as_ref()], &config.program).1;
    assert_eq!(
        protocol::validate(&lane.to_string(), &user, &config),
        Some(Kind::User)
    );
    user[120] ^= 1;
    assert_eq!(protocol::validate(&lane.to_string(), &user, &config), None);
    let mut list = vec![0; 88 + 3 * 32];
    list[..8].copy_from_slice(&Sha256::digest(b"account:List")[..8]);
    list[8..40].copy_from_slice(config.market.as_ref());
    list[72..76].copy_from_slice(&3u32.to_le_bytes());
    list[76..78].copy_from_slice(&100u16.to_le_bytes());
    list[78..80].copy_from_slice(&10000u16.to_le_bytes());
    list[84..88].copy_from_slice(&2u32.to_le_bytes());
    list[88..120].fill(1);
    list[120..152].fill(2);
    assert_eq!(
        protocol::validate(&config.provider_list.to_string(), &list, &config),
        Some(Kind::ProviderList)
    );
    assert_eq!(protocol::list_fees(&list), Some((100, 10000)));
    assert_eq!(protocol::list_members(&list).len(), 64);
    for (offset, value) in [(76, 9u16), (76, 301), (78, 4999), (78, 10001)] {
        let mut invalid = list.clone();
        invalid[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        assert_eq!(protocol::kind(&invalid), None);
    }
    let mut invalid = list.clone();
    invalid[120..152].fill(1);
    assert_eq!(protocol::kind(&invalid), None);
    list[152] = 1;
    assert_eq!(protocol::kind(&list), None);
    let mut native_booking = booking(&config, 5);
    native_booking.resize(472, 0);
    native_booking[..8].copy_from_slice(&Sha256::digest(b"account:Booking")[..8]);
    assert_eq!(protocol::kind(&native_booking), Some(Kind::Booking));
    native_booking.pop();
    assert_eq!(protocol::kind(&native_booking), None);
}
