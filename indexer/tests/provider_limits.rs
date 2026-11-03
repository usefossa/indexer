mod common;
use fossa_indexer::{
    App,
    config::Config,
    index::Record,
    metadata::Document,
    protocol::{self, Kind},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use std::sync::Arc;

fn word(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn limits(config: &Config, provider: Pubkey, page: u8) -> [(String, Vec<u8>); 2] {
    let (page_address, bump) = Pubkey::find_program_address(
        &[b"provider_limit_page", config.market.as_ref(), &[page]],
        &config.program,
    );
    let mut data = vec![0; 88];
    data[..8].copy_from_slice(&protocol::PROVIDER_LIMIT_PAGE_DISCRIMINATOR);
    data[8..40].copy_from_slice(config.market.as_ref());
    data[40] = bump;
    data[41] = page;
    for (at, value) in [
        (44, 1),
        (48, 300),
        (52, 300),
        (56, 60),
        (68, 300),
        (72, 7),
        (76, 30),
        (80, 50),
        (84, 60),
    ] {
        word(&mut data, at, value);
    }
    data[60..68].copy_from_slice(&4_000_000_000i64.to_le_bytes());
    let (identity, bump) = Pubkey::find_program_address(
        &[
            b"provider_limits",
            config.market.as_ref(),
            provider.as_ref(),
        ],
        &config.program,
    );
    let mut index = vec![0; 80];
    index[..8].copy_from_slice(&protocol::PROVIDER_LIMIT_INDEX_DISCRIMINATOR);
    index[8..40].copy_from_slice(config.market.as_ref());
    index[40..72].copy_from_slice(provider.as_ref());
    word(&mut index, 72, u32::from(page));
    index[76] = bump;
    [
        (page_address.to_string(), data),
        (identity.to_string(), index),
    ]
}
fn values(
    rows: &[(String, Vec<u8>)],
    config: &Config,
) -> std::collections::BTreeMap<String, Value> {
    protocol::provider_limit_values(
        rows.iter()
            .map(|(key, data)| (key.as_str(), data.as_slice())),
        config,
    )
}

#[test]
fn generated_provider_limit_accounts_preserve_canonical_abi() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = common::config(dir.path());
    config.market = Pubkey::new_from_array([1; 32]);
    config.program = "routeppVPc5WdqJ2s4foU8QMFYVfQ3HjmucMycBED41"
        .parse()
        .unwrap();
    let mut rows = Vec::new();
    for line in include_str!("fixtures/provider-limits-accounts.jsonl").lines() {
        let fixture: Value = serde_json::from_str(line).unwrap();
        let data: Vec<u8> = serde_json::from_value(fixture["data"].clone()).unwrap();
        let kind = match fixture["layout"].as_str().unwrap() {
            "ProviderLimitPage" => Kind::ProviderLimitPage,
            "ProviderLimitIndex" => Kind::ProviderLimitIndex,
            "ListTerms" => Kind::ListTerms,
            _ => unreachable!(),
        };
        let address = fixture["address"].as_str().unwrap().to_owned();
        assert_eq!(protocol::validate(&address, &data, &config), Some(kind));
        assert_eq!(
            protocol::validate(&Pubkey::new_unique().to_string(), &data, &config),
            None
        );
        assert_eq!(protocol::kind(&data[..data.len() - 1]), None);
        if kind == Kind::ListTerms {
            assert_eq!(data[113], 1);
            assert_eq!(data[234], 2);
        }
        rows.push((address, data));
    }
    assert_eq!(values(&rows, &config).len(), 1);
}

#[test]
fn provider_limits_reject_wrong_bindings_rows_and_windows_without_legacy_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let provider = Pubkey::new_from_array([8; 32]);
    let rows = limits(&config, provider, 7);
    assert_eq!(
        values(&rows, &config)[&provider.to_string()]["providerLimitId"],
        7
    );
    for (row, at, value) in [
        (0, 8, 99),
        (0, 40, 0),
        (0, 41, 32),
        (0, 42, 1),
        (0, 44, 2),
        (0, 56, 0),
        (0, 84, 0),
        (1, 8, 99),
        (1, 40, 99),
        (1, 76, 0),
        (1, 77, 1),
    ] {
        let mut bad = rows.clone();
        bad[row].1[at] = value;
        assert_eq!(
            protocol::validate(&bad[row].0, &bad[row].1, &config),
            None,
            "row {row} offset {at}"
        );
        assert!(values(&bad, &config).is_empty());
    }
    for id in [8, 39, u32::MAX] {
        let mut bad = rows.clone();
        word(&mut bad[1].1, 72, id);
        assert!(values(&bad, &config).is_empty());
    }
    assert!(values(&rows[..1], &config).is_empty());
    assert!(values(&rows[1..], &config).is_empty());
    let other = Pubkey::new_from_array([9; 32]);
    let mut isolated = rows.to_vec();
    isolated.extend(limits(&config, other, 8));
    word(&mut isolated[2].1, 72, 299);
    assert_eq!(
        values(&isolated, &config)[&provider.to_string()]["activeBookings"],
        7
    );
    assert_eq!(
        values(&isolated, &config)[&other.to_string()]["activeBookings"],
        299
    );
    let mut unlimited = rows.clone();
    word(&mut unlimited[0].1, 48, u32::MAX);
    word(&mut unlimited[0].1, 52, 0);
    let value = &values(&unlimited, &config)[&provider.to_string()];
    assert_eq!(value["providerLimits"]["global"]["concurrency"], u32::MAX);
    assert_eq!(value["providerLimits"]["global"]["quota"], 0);
    word(&mut unlimited[0].1, 48, 0);
    assert!(values(&unlimited, &config).is_empty());
    let mut booking = vec![0; protocol::BOOKING_LEN];
    booking[..8].copy_from_slice(&protocol::BOOKING_DISCRIMINATOR);
    booking[72..104].copy_from_slice(config.market.as_ref());
    booking[356] = 3;
    assert_eq!(protocol::kind(&booking), Some(Kind::Booking));
    booking[356] = 4;
    assert_eq!(protocol::kind(&booking), None);
}

fn record(config: &Config, slot: u64, data: Vec<u8>) -> Record {
    Record {
        slot,
        version: slot,
        owner: config.program.to_string(),
        lamports: 1,
        data: data.into(),
    }
}
fn metadata(
    config: &Config,
    owner: Pubkey,
    value: Value,
    provider: bool,
) -> (String, Vec<u8>, String, Document) {
    let seed: &[u8] = if provider {
        b"provider"
    } else {
        b"model_metadata"
    };
    let seeds = if provider {
        vec![seed, config.market.as_ref(), owner.as_ref()]
    } else {
        vec![seed, owner.as_ref()]
    };
    let (address, bump) = Pubkey::find_program_address(&seeds, &config.program);
    let raw = serde_json::to_vec(&value).unwrap();
    let hash = Sha256::digest(&raw);
    let mut data = vec![0; if provider { 364 } else { 396 }];
    data[..8].copy_from_slice(if provider {
        &protocol::PROVIDER_DISCRIMINATOR
    } else {
        &protocol::MODELMETADATA_DISCRIMINATOR
    });
    data[8..40].copy_from_slice(config.market.as_ref());
    data[40..72].copy_from_slice(owner.as_ref());
    let (hash_at, length_at, uri_at, bump_at) = if provider {
        (72, 104, 108, 107)
    } else {
        (104, 136, 140, 138)
    };
    data[hash_at..hash_at + 32].copy_from_slice(&hash);
    let uri = b"https://example.com/metadata.json";
    data[length_at..length_at + 2].copy_from_slice(&(uri.len() as u16).to_le_bytes());
    data[uri_at..uri_at + uri.len()].copy_from_slice(uri);
    data[bump_at] = bump;
    let key = format!(
        "{}:{}",
        if provider { "provider" } else { "model" },
        hex::encode(hash)
    );
    (
        address.to_string(),
        data,
        key,
        Document {
            raw: raw.into(),
            value,
        },
    )
}

struct SharedFixture {
    provider: Pubkey,
    limits: [(String, Vec<u8>); 2],
    books: Vec<(String, Vec<u8>)>,
}

async fn shared_fixture(app: &App, config: &Config) -> SharedFixture {
    let provider = Pubkey::new_from_array([8; 32]);
    let mut market = vec![0; 248];
    market[..8].copy_from_slice(&protocol::MARKET_DISCRIMINATOR);
    market[40..72].copy_from_slice(config.mint.as_ref());
    market[177] = 2;
    let mut list = vec![0; 120];
    list[..8].copy_from_slice(&protocol::LIST_DISCRIMINATOR);
    list[8..40].copy_from_slice(config.market.as_ref());
    word(&mut list, 72, 1);
    list[76..78].copy_from_slice(&100u16.to_le_bytes());
    list[78..80].copy_from_slice(&10000u16.to_le_bytes());
    list[80] = 1;
    word(&mut list, 84, 1);
    list[88..120].copy_from_slice(provider.as_ref());
    let (routing, bump) = Pubkey::find_program_address(
        &[b"list_routing", config.provider_list.as_ref()],
        &config.program,
    );
    let mut admission = vec![0; 140];
    admission[..8].copy_from_slice(&protocol::LIST_ROUTING_DISCRIMINATOR);
    admission[8..40].copy_from_slice(config.market.as_ref());
    admission[40..72].copy_from_slice(config.provider_list.as_ref());
    word(&mut admission, 72, 2);
    admission[76] = bump;
    word(&mut admission, 80, 1);
    admission[84..116].copy_from_slice(provider.as_ref());
    word(&mut admission, 116, 60);
    word(&mut admission, 120, 1);
    word(&mut admission, 124, 1);
    admission[136] = 1;
    let mut rows = vec![
        (config.market.to_string(), market),
        (config.provider_list.to_string(), list),
        (routing.to_string(), admission),
    ];
    let limit_rows = limits(config, provider, 7);
    rows.extend(limit_rows.clone());
    let (address, data, key, doc) = metadata(
        config,
        provider,
        json!({"name":"Example","endpoint":"https://example.com/v1/chat/completions"}),
        true,
    );
    rows.push((address, data));
    let mut documents = vec![(key, doc)];
    let mut books = Vec::new();
    for id in ["example/first", "example/second"] {
        let hash = Sha256::digest(id.as_bytes());
        let (model, bump) = Pubkey::find_program_address(
            &[b"model", config.market.as_ref(), &hash],
            &config.program,
        );
        let (terms, capacity) =
            protocol::list_books(&config.program, &config.provider_list, &model);
        let mut data = vec![0; 176];
        data[..8].copy_from_slice(&protocol::MODEL_DISCRIMINATOR);
        data[8..40].copy_from_slice(config.market.as_ref());
        data[40..72].copy_from_slice(&hash);
        data[72..104].copy_from_slice(Pubkey::new_from_array([20; 32]).as_ref());
        data[104..136].copy_from_slice(Pubkey::new_from_array([21; 32]).as_ref());
        word(&mut data, 136, 1);
        data[140] = bump;
        rows.push((model.to_string(), data));
        let (address, data, key, doc) = metadata(
            config,
            model,
            json!({"id":id,"name":"Example Model","checkpoints":[{"id":"revision","name":"Revision"}],"modalities":{"input":["text"],"output":["text"]},"contextTokens":1000,"outputTokens":100}),
            false,
        );
        rows.push((address, data));
        documents.push((key, doc));
        let mut data = vec![0; 260];
        data[..8].copy_from_slice(&protocol::LIST_TERMS_DISCRIMINATOR);
        data[8..40].copy_from_slice(model.as_ref());
        data[40..72].copy_from_slice(config.provider_list.as_ref());
        data[72..104].copy_from_slice(capacity.parse::<Pubkey>().unwrap().as_ref());
        word(&mut data, 104, 1);
        data[112] = Pubkey::find_program_address(
            &[b"list_terms", config.provider_list.as_ref(), model.as_ref()],
            &config.program,
        )
        .1;
        data[113] = 1;
        word(&mut data, 116, 1);
        data[120..152].copy_from_slice(provider.as_ref());
        data[152..184].copy_from_slice(provider.as_ref());
        data[184..216].copy_from_slice(&Sha256::digest(b"revision"));
        word(&mut data, 216, 100);
        word(&mut data, 224, 200);
        data[231] = 1;
        data[232] = 1;
        data[233] = 1;
        data[234] = 2;
        for (at, value) in [(236, 1), (240, 1000), (244, 900), (248, 100), (252, 7)] {
            word(&mut data, at, value);
        }
        books.push((terms.clone(), data.clone()));
        rows.push((terms, data));
    }
    for (address, data) in &rows {
        assert!(
            protocol::validate(address, data, config).is_some(),
            "{address}"
        );
    }
    app.index
        .apply(
            &app.db,
            config,
            rows.into_iter()
                .map(|(address, data)| (address, record(config, 10, data)))
                .collect(),
            Some(10),
        )
        .await
        .unwrap();
    app.index.state.write().await.documents.extend(documents);
    app.index.connected(true).await;
    app.index.activity().await;
    SharedFixture {
        provider,
        limits: limit_rows,
        books,
    }
}

#[tokio::test]
async fn shared_catalog_uses_canonical_limits_across_models_and_invalidates_updates() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    let SharedFixture {
        provider,
        limits: limit_rows,
        books,
    } = shared_fixture(&app, &config).await;
    let catalog = app.index.catalog(&config).await;
    assert!(catalog.ready);
    assert_eq!(catalog.value["providerLimitsEnabled"], true);
    assert_eq!(catalog.value["marketConfig"]["listRoutingEnabled"], true);
    assert_eq!(catalog.value["models"].as_array().unwrap().len(), 2);
    for model in catalog.value["models"].as_array().unwrap() {
        let offer = &model["offers"][0];
        assert_eq!(offer["capacity"], 300);
        assert_eq!(offer["activeBookings"], 7);
        assert_eq!(offer["providerLimitId"], 7);
        assert_eq!(offer["available"], true);
        assert_eq!(offer["provider"]["quotaUsed"], 300);
        assert_eq!(offer["provider"]["quotaWindowEnd"], 4_000_000_000i64);
        assert_eq!(
            offer["provider"]["bookingLimits"]["walletBookingCapacity"],
            30
        );
        assert_eq!(offer["provider"]["providerLimits"]["wallet"]["quota"], 50);
    }
    assert_eq!(
        catalog.directory.providers[&provider.to_string()]["activeBookings"],
        7
    );
    let mut page = limit_rows[0].1.clone();
    word(&mut page, 72, 300);
    app.index
        .apply(
            &app.db,
            &config,
            vec![(limit_rows[0].0.clone(), record(&config, 11, page))],
            None,
        )
        .await
        .unwrap();
    let full = app.index.catalog(&config).await;
    assert!(!Arc::ptr_eq(&catalog, &full));
    assert_eq!(
        full.directory.providers[&provider.to_string()]["activeBookings"],
        300
    );
    assert!(
        full.value["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model["offers"][0]["available"] == false)
    );
    for (step, (address, mut data)) in [limit_rows[1].clone(), books[0].clone(), books[1].clone()]
        .into_iter()
        .enumerate()
    {
        if step == 0 {
            word(&mut data, 72, 39);
        } else if step == 1 {
            data[113] = 0;
            data[234] = 1;
        } else {
            word(&mut data, 252, 8);
        }
        app.index
            .apply(
                &app.db,
                &config,
                vec![(address.clone(), record(&config, 12 + step as u64 * 2, data))],
                None,
            )
            .await
            .unwrap();
        let missing = app.index.catalog(&config).await;
        assert!(!missing.ready);
        assert!(Arc::ptr_eq(&missing, &app.index.catalog(&config).await));
        assert!(
            missing.value["models"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|model| model["offers"].as_array().unwrap())
                .all(|offer| offer["available"] == false)
        );
        let original = if step == 0 {
            limit_rows[1].1.clone()
        } else {
            books[step - 1].1.clone()
        };
        app.index
            .apply(
                &app.db,
                &config,
                vec![(address, record(&config, 13 + step as u64 * 2, original))],
                None,
            )
            .await
            .unwrap();
        assert!(app.index.catalog(&config).await.ready);
    }
    app.index
        .apply(
            &app.db,
            &config,
            vec![(limit_rows[0].0.clone(), Record::closed(20, 20))],
            None,
        )
        .await
        .unwrap();
    let missing = app.index.catalog(&config).await;
    assert!(!missing.ready);
    assert!(missing.value["models"].as_array().unwrap().is_empty());
    assert!(missing.directory.providers[&provider.to_string()]["bookingLimits"].is_null());
}

#[tokio::test]
async fn shared_catalog_requires_metadata_only_for_active_admitted_offers() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path());
    let app = App::new(config.clone()).await.unwrap();
    let fixture = shared_fixture(&app, &config).await;
    let provider = Pubkey::new_from_array([9; 32]);
    let routing = Pubkey::find_program_address(
        &[b"list_routing", config.provider_list.as_ref()],
        &config.program,
    )
    .0
    .to_string();
    let state = app.index.state.read().await;
    let mut list = state.accounts[&config.provider_list.to_string()]
        .data
        .to_vec();
    let mut admission = state.accounts[&routing].data.to_vec();
    drop(state);
    list.extend_from_slice(provider.as_ref());
    word(&mut list, 72, 2);
    word(&mut list, 84, 2);
    admission.extend_from_within(84..140);
    admission[140..172].copy_from_slice(provider.as_ref());
    word(&mut admission, 80, 2);
    let mut rows = vec![
        (config.provider_list.to_string(), list),
        (routing, admission),
    ];
    for (address, mut data) in fixture.books {
        data.extend_from_within(120..260);
        data[260..292].copy_from_slice(provider.as_ref());
        data[292..324].copy_from_slice(provider.as_ref());
        for (at, value) in [
            (104, 2),
            (108, 2),
            (116, 2),
            (356, 1),
            (364, 2),
            (392, 8),
            (396, 1),
        ] {
            word(&mut data, at, value);
        }
        rows.push((address, data));
    }
    let (profile, data, key, document) = metadata(
        &config,
        provider,
        json!({"name":"Second Provider","endpoint":"https://example.org/v1/chat/completions"}),
        true,
    );
    let profile_row = (profile.clone(), data);
    let mut setup = rows.clone();
    setup.push(profile_row.clone());
    setup.extend(limits(&config, provider, 8));
    setup.push(fixture.limits[0].clone());
    for (_, data) in &mut setup {
        if protocol::kind(data) == Some(Kind::ProviderLimitPage) {
            word(data, 68, 7);
        }
    }
    app.index
        .apply(
            &app.db,
            &config,
            setup
                .into_iter()
                .map(|(address, data)| (address, record(&config, 11, data)))
                .collect(),
            None,
        )
        .await
        .unwrap();
    let missing = app.index.catalog(&config).await;
    assert!(!missing.ready);
    assert!(Arc::ptr_eq(&missing, &app.index.catalog(&config).await));
    assert_eq!(missing.directory.provider_limits.len(), 2);
    for model in missing.value["models"].as_array().unwrap() {
        assert_eq!(model["offerCount"], 2);
        assert_eq!(model["offers"].as_array().unwrap().len(), 1);
        assert_eq!(model["offers"][0]["available"], false);
    }
    for (step, reason) in ["removed", "frozen", "inactive"].into_iter().enumerate() {
        let mut updates = rows.clone();
        match reason {
            "removed" => {
                word(&mut updates[0].1, 84, 1);
                updates[0].1[120..152].fill(0);
            }
            "frozen" => updates[1].1[192] = 0,
            _ => {
                for (_, data) in &mut updates[2..] {
                    data[371] = 0;
                }
            }
        }
        app.index
            .apply(
                &app.db,
                &config,
                updates
                    .into_iter()
                    .map(|(address, data)| (address, record(&config, 12 + step as u64, data)))
                    .collect(),
                None,
            )
            .await
            .unwrap();
        let catalog = app.index.catalog(&config).await;
        assert!(catalog.ready, "{reason}");
        assert_eq!(catalog.directory.provider_limits.len(), 2);
        for model in catalog.value["models"].as_array().unwrap() {
            assert_eq!(model["offerCount"], 2);
            assert_eq!(model["offers"].as_array().unwrap().len(), 1);
            assert_eq!(model["offers"][0]["available"], true, "{reason}");
        }
    }
    app.index
        .apply(
            &app.db,
            &config,
            rows.into_iter()
                .map(|(address, data)| (address, record(&config, 15, data)))
                .collect(),
            None,
        )
        .await
        .unwrap();
    let missing = app.index.catalog(&config).await;
    assert!(!missing.ready);
    {
        let mut state = app.index.state.write().await;
        state.documents.insert(key, document);
        state.cached = None;
        state.directory.take();
    }
    let complete = app.index.catalog(&config).await;
    assert!(complete.ready);
    assert!(!Arc::ptr_eq(&missing, &complete));
    for model in complete.value["models"].as_array().unwrap() {
        let offers = model["offers"].as_array().unwrap();
        assert_eq!(offers.len(), 2);
        assert!(offers.iter().all(|offer| offer["available"] == true));
        assert_eq!(offers[1]["provider"]["wallet"], provider.to_string());
        assert!(offers[1]["inputRate"].as_u64() < offers[0]["inputRate"].as_u64());
    }
    app.index
        .apply(
            &app.db,
            &config,
            vec![(profile.clone(), Record::closed(16, 16))],
            None,
        )
        .await
        .unwrap();
    assert!(!app.index.catalog(&config).await.ready);
    app.index
        .apply(
            &app.db,
            &config,
            vec![(profile, record(&config, 17, profile_row.1))],
            None,
        )
        .await
        .unwrap();
    assert!(app.index.catalog(&config).await.ready);
}
