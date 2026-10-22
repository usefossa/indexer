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
async fn verified_catalog_requires_canonical_pdas_hash_book_links_and_exact_collateral() {
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
    let mut rows = vec![(config.market.to_string(), market(&config))];
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
            data[106] = 1;
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
    data[136..140].copy_from_slice(&1u32.to_le_bytes());
    data[140] = model_bump;
    assert_eq!(
        protocol::validate(&model.to_string(), &data, &config),
        Some(Kind::Model)
    );
    assert!(protocol::validate(&wallet.to_string(), &data, &config).is_none());
    rows.push((model.to_string(), data));
    for (address, len, magic) in [(terms, 168, b"TERMSV20"), (capacity, 80, b"CAPACV20")] {
        let mut data = vec![0; len];
        data[..8].copy_from_slice(magic);
        data[8..40].copy_from_slice(model.as_ref());
        data[40..44].copy_from_slice(&1u32.to_le_bytes());
        data[44..48].copy_from_slice(&1u32.to_le_bytes());
        if len == 168 {
            data[48..80].copy_from_slice(wallet.as_ref());
            data[144..148].copy_from_slice(&100u32.to_le_bytes());
            data[152..156].copy_from_slice(&200u32.to_le_bytes());
            data[156..158].copy_from_slice(&50u16.to_le_bytes());
            data[159] = 1;
            data[160] = 1;
            data[164..168].copy_from_slice(&5u32.to_le_bytes());
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
        catalog.value["models"][0]["offers"][0]["fundedBond"],
        "9007199254740993"
    );
    assert_eq!(catalog.value["models"][0]["offers"][0]["available"], true);
    app.index.connected(false).await;
    let catalog = app.index.catalog(&config).await;
    assert!(!catalog.ready);
    assert_eq!(catalog.value["models"][0]["offers"][0]["available"], false);
    assert!(
        protocol::bytes(&json!({"data":[STANDARD.encode([0u8;10]),"base64"],"executable":true}))
            .is_none()
    );
}
