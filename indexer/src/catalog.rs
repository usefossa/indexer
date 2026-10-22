use crate::{
    config::Config,
    index::State,
    metadata,
    protocol::{self, Kind, address, u16_at, u32_at, u64_at},
};
use axum::{body::Bytes, extract::ws::Utf8Bytes};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub struct Catalog {
    pub ready: bool,
    pub value: Value,
    pub body: Bytes,
    pub message: Utf8Bytes,
}
pub fn build(index: &State, config: &Config) -> Catalog {
    let ready = index.ready(config);
    let mut providers = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    for (id, r) in &index.accounts {
        match protocol::kind(&r.data) {
            Some(Kind::Provider) => {
                if let Some(doc) = metadata::key(&r.data).and_then(|k| index.documents.get(&k)) {
                    let wallet = address(&r.data, 40);
                    providers.insert(wallet.clone(),json!({"wallet":wallet,"address":id,"name":doc.value["name"],"endpoint":doc.value["endpoint"],"image":doc.value.get("image"),"approved":r.data[106]==1}));
                }
            }
            Some(Kind::Metadata) => {
                metadata.insert(address(&r.data, 40), r);
            }
            _ => {}
        }
    }
    let mut models = Vec::new();
    for (id, r) in &index.accounts {
        let d = &r.data;
        if protocol::kind(d) != Some(Kind::Model) {
            continue;
        }
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
        let terms_id = address(d, 72);
        let capacity_id = address(d, 104);
        let (Some(terms), Some(capacity)) = (
            index.accounts.get(&terms_id),
            index.accounts.get(&capacity_id),
        ) else {
            continue;
        };
        let (t, c) = (&terms.data, &capacity.data);
        let limit = u32_at(d, 136);
        if protocol::kind(t) != Some(Kind::Terms)
            || protocol::kind(c) != Some(Kind::Capacity)
            || address(t, 8) != *id
            || address(c, 8) != *id
            || u32_at(t, 40) != limit
            || u32_at(c, 40) != limit
            || u32_at(t, 44) != u32_at(c, 44)
        {
            continue;
        }
        let mut offers = Vec::new();
        for slot in 0..u32_at(t, 44) as usize {
            let ti = 48 + slot * 120;
            let ci = 48 + slot * 32;
            let wallet = address(t, ti);
            let provider = providers.get(&wallet).cloned().unwrap_or_else(
                || json!({"wallet":wallet,"name":"Unknown provider","approved":false}),
            );
            let funded = u64_at(c, ci);
            let exposure = u64_at(c, ci + 8);
            let active = u32_at(c, ci + 16);
            let capacity = u32_at(t, ti + 116);
            let available = ready
                && t[ti + 111] == 1
                && t[ti + 112] == 1
                && provider["approved"] == true
                && active < capacity
                && funded > exposure;
            offers.push(json!({"slot":slot,"provider":provider,"payout":address(t,ti+32),"bondVault":address(t,ti+64),"inputRate":u32_at(t,ti+96),"cachedRate":u32_at(t,ti+100),"outputRate":u32_at(t,ti+104),"speed":u16_at(t,ti+108),"private":t[ti+110]==1,"active":t[ti+111]==1,"approved":t[ti+112]==1,"capacity":capacity,"activeBookings":active,"fundedBond":funded.to_string(),"exposure":exposure.to_string(),"available":available}));
        }
        models.push(json!({"id":model_id,"address":id,"name":doc["name"],"terms":terms_id,"capacity":capacity_id,"limit":limit,"offers":offers,"modalities":doc["modalities"],"effort":doc.get("effort"),"variants":doc.get("variants"),"settings":doc.get("settings"),"capabilities":doc.get("capabilities").cloned().unwrap_or_else(||json!({"tools":false,"reasoning":doc.get("effort").is_some()})),"contextTokens":doc["contextTokens"],"outputTokens":doc["outputTokens"],"inputBoundTokens":doc.get("inputBoundTokens").unwrap_or(&doc["contextTokens"])}));
    }
    models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let pending = index
        .accounts
        .values()
        .filter_map(|r| metadata::key(&r.data))
        .filter(|key| !index.documents.contains_key(key))
        .count();
    let value = json!({"ready":ready,"slot":index.slot,"programId":config.program.to_string(),"market":config.market.to_string(),"models":models,"metadataPending":pending,"metadataVerified":index.documents.len(),"providers":providers.into_values().collect::<Vec<_>>()});
    let body: Bytes = value.to_string().into();
    let message = format!("{{\"type\":\"catalog\",\"data\":{}}}", value).into();
    Catalog {
        ready,
        value,
        body,
        message,
    }
}
