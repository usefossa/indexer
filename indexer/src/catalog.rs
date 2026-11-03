use crate::{
    config::Config,
    index::State,
    metadata,
    protocol::{self, Kind, address, u16_at, u32_at},
};
use axum::{body::Bytes, extract::ws::Utf8Bytes};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub struct Catalog {
    pub ready: bool,
    pub source_ready: bool,
    pub directory: std::sync::Arc<crate::profiles::Directory>,
    pub value: Value,
    pub body: Bytes,
    pub message: Utf8Bytes,
}
pub fn build(index: &State, config: &Config) -> Catalog {
    let source_ready = index.catalog_ready(config);
    let directory = index
        .directory
        .get_or_init(|| std::sync::Arc::new(crate::profiles::Directory::build(index, config)))
        .clone();
    let list = index
        .accounts
        .get(&config.provider_list.to_string())
        .filter(|r| {
            protocol::validate(&config.provider_list.to_string(), &r.data, config)
                == Some(Kind::ProviderList)
        });
    let members: std::collections::HashSet<&[u8]> = list
        .map(|r| protocol::list_members(&r.data).chunks_exact(32).collect())
        .unwrap_or_default();
    let mut providers = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let booking_limits =
        protocol::booking_limits(index.accounts.values().map(|record| record.data.as_ref()));
    let mode = index
        .accounts
        .get(&config.market.to_string())
        .and_then(|record| record.data.get(177))
        .copied()
        .unwrap_or(0);
    let scoped = mode >= 1;
    let shared = mode == 2;
    let routing_address = solana_pubkey::Pubkey::find_program_address(
        &[b"list_routing", config.provider_list.as_ref()],
        &config.program,
    )
    .0
    .to_string();
    let routing = index
        .accounts
        .get(&routing_address)
        .filter(|_| list.is_some_and(|record| record.data.get(80) == Some(&1)))
        .filter(|record| protocol::address(&record.data, 40) == config.provider_list.to_string());
    let routing_limits = routing.and_then(|record| protocol::routing_policy_values(&record.data));
    let ready = source_ready
        && (!scoped || routing_limits.is_some())
        && (!shared
            || routing.is_some_and(|routing| {
                shared_books_ready(
                    index,
                    config,
                    &directory.provider_limits,
                    u32_at(&routing.data, 72),
                )
            }));
    for (id, r) in &index.accounts {
        match protocol::kind(&r.data) {
            Some(Kind::Provider) if members.contains(&r.data[40..72]) => {
                if let Some(doc) = metadata::key(&r.data).and_then(|k| index.documents.get(&k)) {
                    let wallet = address(&r.data, 40);
                    let limits = if shared {
                        directory
                            .provider_limits
                            .get(&wallet)
                            .map(|value| &value["bookingLimits"])
                    } else if scoped {
                        routing_limits
                            .as_ref()
                            .and_then(|limits| limits.get(&wallet))
                    } else {
                        booking_limits.get(&wallet)
                    };
                    let mut value = json!({"wallet":wallet,"address":id,"name":doc.value["name"],"endpoint":doc.value["endpoint"],"image":doc.value.get("image"),"requestLimits":doc.value.get("requestLimits"),"bookingLimits":limits});
                    if shared && let Some(limits) = directory.provider_limits.get(&wallet) {
                        value
                            .as_object_mut()
                            .unwrap()
                            .extend(limits.as_object().unwrap().clone());
                    }
                    providers.insert(wallet, value);
                }
            }
            Some(Kind::Metadata) => {
                metadata.insert(address(&r.data, 40), r);
            }
            _ => {}
        }
    }
    let mut models = Vec::new();
    let mut aliases = BTreeMap::new();
    let groups: BTreeMap<_, _> = index
        .accounts
        .iter()
        .filter(|(_, record)| protocol::kind(&record.data) == Some(Kind::ModelGroup))
        .map(|(id, record)| (id.clone(), protocol::model_group(&record.data, id)))
        .collect();
    let market_config = index
        .accounts
        .get(&config.market.to_string())
        .and_then(|record| protocol::market_config(&record.data));
    for (id, r) in &index.accounts {
        let d = &r.data;
        if protocol::kind(d) != Some(Kind::Model) {
            continue;
        }
        let group = (d.len() == 176)
            .then(|| protocol::optional_address(d, 144))
            .flatten();
        let group_ready = group
            .as_ref()
            .is_none_or(|group| groups.contains_key(group));
        let Some(doc) = metadata
            .get(id)
            .and_then(|r| metadata::key(&r.data))
            .and_then(|key| index.documents.get(&key))
            .map(|d| &d.value)
        else {
            continue;
        };
        let Some(model_id) = doc["id"].as_str() else {
            continue;
        };
        if Sha256::digest(model_id.as_bytes()).as_slice() != &d[40..72] {
            continue;
        }
        if let Some(target) = doc.get("aliasOf").and_then(Value::as_str) {
            aliases.insert(model_id.to_owned(), target.to_owned());
            continue;
        }
        let (terms_id, capacity_id) = if scoped {
            let Ok(model) = id.parse::<solana_pubkey::Pubkey>() else {
                continue;
            };
            protocol::list_books(&config.program, &config.provider_list, &model)
        } else {
            (address(d, 72), address(d, 104))
        };
        let Some(terms) = index.accounts.get(&terms_id) else {
            continue;
        };
        let t = &terms.data;
        let c = index
            .accounts
            .get(&capacity_id)
            .map(|record| record.data.as_ref())
            .unwrap_or_default();
        let limit = u32_at(if scoped { t } else { d }, if scoped { 104 } else { 136 });
        let quota_terms = t.starts_with(&protocol::BOOKING_TERMS_DISCRIMINATOR);
        let count = if shared {
            if protocol::kind(t) != Some(Kind::ListTerms)
                || t[113] != 1
                || address(t, 8) != *id
                || address(t, 40) != config.provider_list.to_string()
                || address(t, 72) != capacity_id
            {
                continue;
            }
            u32_at(t, 116)
        } else if scoped {
            if protocol::kind(t) != Some(Kind::ListTerms)
                || protocol::kind(c) != Some(Kind::ListCapacity)
                || address(t, 8) != *id
                || address(c, 8) != *id
                || address(t, 40) != config.provider_list.to_string()
                || address(c, 40) != config.provider_list.to_string()
                || address(t, 72) != capacity_id
                || address(c, 72) != terms_id
                || u32_at(t, 104) != u32_at(c, 104)
                || u32_at(t, 116) != u32_at(c, 112)
            {
                continue;
            }
            u32_at(t, 116)
        } else {
            if !quota_terms && !t.starts_with(&protocol::TERMS_DISCRIMINATOR)
                || protocol::kind(t) != Some(Kind::Terms)
                || protocol::kind(c) != Some(Kind::Capacity)
                || address(t, 8) != *id
                || address(c, 8) != *id
                || u32_at(t, 40) != limit
                || u32_at(c, 40) != limit
                || u32_at(t, 44) != u32_at(c, 44)
            {
                continue;
            }
            u32_at(t, 44)
        };
        let checkpoints: BTreeMap<[u8; 32], _> = doc
            .get("checkpoints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(order, revision)| {
                revision["id"].as_str().map(|id| {
                    (
                        Sha256::digest(id.as_bytes()).into(),
                        (
                            revision
                                .get("ordered")
                                .and_then(Value::as_bool)
                                .unwrap_or(true)
                                .then_some(order),
                            revision,
                        ),
                    )
                })
            })
            .collect();
        let mut offers = Vec::new();
        for slot in 0..count as usize {
            let ti = if scoped { 120 } else { 48 }
                + slot
                    * if scoped {
                        140
                    } else if quota_terms {
                        136
                    } else {
                        protocol::TERMS_LEN
                    };
            let checkpoint = &t[ti + 64..ti + 96];
            let Some((checkpoint_order, revision)) = checkpoints.get(checkpoint) else {
                continue;
            };
            let input_modalities = t[ti + 112];
            let output_modalities = t[ti + 113];
            let context_tokens = u32_at(t, ti + 120);
            let input_bound_tokens = u32_at(t, ti + 124);
            let output_tokens = u32_at(t, ti + 128);
            if t[ti + 114] != if shared { 2 } else { 1 }
                || t[ti + 115] != 0
                || input_modalities == 0
                || input_modalities & !15 != 0
                || output_modalities == 0
                || output_modalities & !15 != 0
                || input_bound_tokens == 0
                || output_tokens == 0
                || input_bound_tokens > context_tokens
                || output_tokens > context_tokens
            {
                continue;
            }
            let ci = if scoped {
                116 + slot * 36
            } else {
                48 + slot * 32
            };
            let wallet = address(t, ti);
            let Some(provider) = providers.get(&wallet) else {
                continue;
            };
            if scoped
                && ((!shared && address(c, ci) != wallet)
                    || routing_limits
                        .as_ref()
                        .is_none_or(|limits| !limits.contains_key(&wallet)))
            {
                continue;
            }
            if quota_terms
                && provider["bookingLimits"]["providerPolicy"].as_u64()
                    != Some(u64::from(u32_at(t, ti + 132)))
            {
                continue;
            }
            if shared
                && provider["providerLimitId"].as_u64() != Some(u64::from(u32_at(t, ti + 132)))
            {
                continue;
            }
            let active = if shared {
                provider["activeBookings"].as_u64().unwrap_or(0) as u32
            } else {
                u32_at(c, ci + if scoped { 32 } else { 16 })
            };
            let capacity = if shared {
                provider["providerLimits"]["global"]["concurrency"]
                    .as_u64()
                    .unwrap_or(0) as u32
            } else {
                u32_at(t, ti + 116)
            };
            let available = ready
                && group_ready
                && t[ti + 111] == 1
                && provider["endpoint"].as_str().is_some_and(|s| !s.is_empty())
                && active < capacity;
            offers.push(json!({
                "checkpoint": hex::encode(checkpoint),
                "checkpointId": revision["id"], "checkpointName": revision["name"],
                "checkpointOrder": checkpoint_order,
                "inputModalities": input_modalities, "outputModalities": output_modalities,
                "modalities": {"input": modality_names(input_modalities), "output": modality_names(output_modalities)},
                "contextTokens": context_tokens, "inputBoundTokens": input_bound_tokens,
                "outputTokens": output_tokens,
                "slot": slot, "provider": provider, "payout": address(t, ti + 32),
                "inputRate": u32_at(t, ti + 96), "cachedRate": u32_at(t, ti + 100),
                "outputRate": u32_at(t, ti + 104), "speed": u16_at(t, ti + 108),
                "private": t[ti + 110] == 1, "active": t[ti + 111] == 1,
                "capacity": capacity, "activeBookings": active, "available": available,
                "providerLimitId": if shared {provider.get("providerLimitId")} else {None}
            }));
        }
        if offers.is_empty() {
            continue;
        }
        let creator = doc.get("creator").and_then(Value::as_str).filter(|value| {
            !value.is_empty()
                && value.len() <= 256
                && !value.chars().any(|c| c.is_whitespace() || c.is_control())
        });
        models.push(json!({"id":model_id,"creator":creator,"address":id,"group":if d.len() == 176 && d[144..176] != [0;32] {Some(address(d,144))} else {None},"name":doc["name"],"terms":terms_id,"capacity":capacity_id,"limit":limit,"offerCount":count,"offers":offers,"modalities":doc["modalities"],"effort":doc.get("effort"),"variants":doc.get("variants"),"settings":doc.get("settings"),"capabilities":doc.get("capabilities").cloned().unwrap_or_else(||json!({"tools":false,"reasoning":doc.get("effort").is_some()})),"contextTokens":doc["contextTokens"],"outputTokens":doc["outputTokens"],"inputBoundTokens":doc.get("inputBoundTokens").unwrap_or(&doc["contextTokens"])}));
    }
    models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    aliases.retain(|_, target| {
        models
            .iter()
            .any(|model| model["id"].as_str() == Some(target.as_str()))
    });
    let pending = index
        .accounts
        .values()
        .filter_map(|r| metadata::key(&r.data))
        .filter(|key| !index.documents.contains_key(key))
        .count();
    let value = json!({"aliases":aliases,"ready":ready,"slot":index.slot,"programId":config.program.to_string(),"market":config.market.to_string(),"marketConfig":market_config,"listRoutingEnabled":scoped,"providerLimitsEnabled":shared,"listRouting":solana_pubkey::Pubkey::find_program_address(&[b"list_routing",config.provider_list.as_ref()],&config.program).0.to_string(),"modelGroups":groups.into_values().collect::<Vec<_>>(),"providerList":config.provider_list.to_string(),"providerListFeeBps":list.and_then(|r|protocol::list_fees(&r.data)).map(|fees|fees.0),"providerListProtocolShareBps":list.and_then(|r|protocol::list_fees(&r.data)).map(|fees|fees.1),"models":models,"metadataPending":pending,"metadataVerified":index.documents.len(),"providers":providers.into_values().collect::<Vec<_>>()});
    let body: Bytes = value.to_string().into();
    let message = format!("{{\"type\":\"catalog\",\"data\":{}}}", value).into();
    Catalog {
        ready,
        source_ready,
        directory,
        value,
        body,
        message,
    }
}

fn shared_books_ready(
    index: &State,
    config: &Config,
    limits: &BTreeMap<String, Value>,
    expected: u32,
) -> bool {
    let mut count = 0;
    for record in index.accounts.values() {
        let data = &record.data;
        if protocol::kind(data) != Some(Kind::ListTerms)
            || protocol::key(data, 40) != Some(config.provider_list)
        {
            continue;
        }
        count += 1;
        if data[113] != 1
            || data[120..120 + u32_at(data, 116) as usize * 140]
                .chunks_exact(140)
                .any(|row| {
                    row[114] != 2
                        || limits
                            .get(&address(row, 0))
                            .and_then(|value| value["providerLimitId"].as_u64())
                            != Some(u64::from(u32_at(row, 132)))
                })
        {
            return false;
        }
    }
    count == expected
}

fn modality_names(mask: u8) -> Vec<&'static str> {
    [(1, "text"), (2, "image"), (4, "audio"), (8, "video")]
        .into_iter()
        .filter_map(|(bit, name)| (mask & bit != 0).then_some(name))
        .collect()
}
