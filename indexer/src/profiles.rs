use crate::{
    App,
    error::{Error, Result},
    index::State as IndexState,
    metadata,
    protocol::{self, Kind, address, u32_at},
    usage,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};

fn short(address: &str) -> String {
    if address.len() > 12 {
        format!("{}…{}", &address[..4], &address[address.len() - 4..])
    } else {
        address.to_owned()
    }
}
pub struct List {
    pub summary: Value,
    data: Arc<[u8]>,
    hash: String,
}
impl List {
    fn members(&self) -> impl Iterator<Item = &[u8]> {
        self.data[80..80 + u32_at(&self.data, 76) as usize * 32].chunks_exact(32)
    }
    fn contains(&self, needle: &[u8]) -> bool {
        let keys = &self.data[80..80 + u32_at(&self.data, 76) as usize * 32];
        let (mut low, mut high) = (0, keys.len() / 32);
        while low < high {
            let mid = low + (high - low) / 2;
            match keys[mid * 32..mid * 32 + 32].cmp(needle) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }
}
pub struct Directory {
    pub providers: HashMap<String, Value>,
    pub models: HashMap<String, String>,
    pub lists: HashMap<String, List>,
}
impl Directory {
    pub fn build(index: &IndexState) -> Self {
        let (mut providers, mut models, mut metadata) =
            (HashMap::new(), HashMap::new(), HashMap::new());
        for (id, record) in &index.accounts {
            let d = &record.data;
            let Some(doc) = metadata::key(d).and_then(|k| index.documents.get(&k)) else {
                continue;
            };
            match protocol::kind(d) {
                Some(Kind::Provider) => {
                    let wallet = address(d, 40);
                    providers.insert(wallet.clone(),json!({"wallet":wallet,"address":id,"name":doc.value["name"],"endpoint":doc.value["endpoint"],"image":doc.value.get("image"),"website":doc.value.get("website")}));
                }
                Some(Kind::Metadata) => {
                    let model = address(d, 40);
                    if index.accounts.get(&model).is_some_and(|m| {
                        protocol::kind(&m.data) == Some(Kind::Model)
                            && doc.value["id"].as_str().is_some_and(|id| {
                                Sha256::digest(id.as_bytes()).as_slice() == &m.data[40..72]
                            })
                    }) {
                        models.insert(
                            model,
                            doc.value["name"].as_str().unwrap_or_default().to_owned(),
                        );
                    }
                }
                Some(Kind::ListMetadata) => {
                    metadata.insert(address(d, 40), (d, &doc.value));
                }
                _ => {}
            }
        }
        let lists=index.accounts.iter().filter_map(|(id,record)| {
            let d=&record.data;
            if protocol::kind(d)!=Some(Kind::ProviderList) {return None;}
            let doc=metadata.get(id).filter(|(md,_)|md[8..40]==d[8..40]&&md[72..104]==d[40..72]).map(|(_,doc)|*doc);
            let value=|key:&str|doc.and_then(|d|d.get(key)).cloned().unwrap_or(Value::Null);
            let summary=json!({"address":id,"authority":address(d,40),"name":doc.and_then(|d|d["name"].as_str()).map(str::to_owned).unwrap_or_else(||short(id)),"image":value("image"),"icon":value("icon"),"website":value("website"),"accentColor":doc.and_then(|d|d["accentColor"].as_str()).unwrap_or("#94A3B8"),"providerCount":u32_at(d,76)});
            Some((id.clone(),List{summary,data:d.clone(),hash:metadata.get(id).map(|(d,_)|hex::encode(&d[104..136])).unwrap_or_default()}))
        }).collect();
        Self {
            providers,
            models,
            lists,
        }
    }
    fn summary(&self, id: &str, list: &List, rank: usize, limit: usize, base: &str) -> Value {
        let mut value = list.summary.clone();
        value["rank"] = rank.into();
        if value["icon"].is_string() {
            value["icon"] =
                format!("{base}/v1/market/lists/{id}/icon?version={}", list.hash).into();
        }
        value["providers"]=list.members().take(limit).map(|k| {
            let wallet=bs58::encode(k).into_string();
            json!({"wallet":wallet,"name":self.providers.get(&wallet).and_then(|p|p["name"].as_str()).map(str::to_owned).unwrap_or_else(||short(&wallet))})
        }).collect();
        value
    }
    fn ranked<'a>(&'a self, usage: &HashMap<String, u64>) -> Vec<(&'a String, &'a List)> {
        let mut lists = self.lists.iter().collect::<Vec<_>>();
        lists.sort_by(|a, b| {
            usage
                .get(b.0)
                .copied()
                .unwrap_or(0)
                .cmp(&usage.get(a.0).copied().unwrap_or(0))
                .then_with(|| a.0.cmp(b.0))
        });
        lists
    }
    fn names(&self, stats: &mut Value) {
        stats["topModel"] = stats["topModel"]
            .as_str()
            .map(|id| {
                self.models
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_owned())
            })
            .into();
        stats["topProvider"] = stats["topProvider"]
            .as_str()
            .map(|id| {
                self.providers
                    .get(id)
                    .and_then(|p| p["name"].as_str())
                    .unwrap_or(id)
                    .to_owned()
            })
            .into();
    }
}
#[derive(Default, Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
}
pub async fn lists(State(app): State<App>, Query(query): Query<Search>) -> Result<Json<Value>> {
    if query.q.len() > 128 {
        return Err(Error::bad("Search is too long"));
    }
    let catalog = app.index.catalog(&app.config).await;
    let (usage_ready, usage) = usage::rankings(&app).await?;
    let directory = &catalog.directory;
    let query = query.q.trim().to_lowercase();
    let lists: Vec<_> = directory
        .ranked(&usage)
        .into_iter()
        .enumerate()
        .filter(|(_, (id, list))| {
            query.is_empty()
                || id.to_lowercase().contains(&query)
                || list.summary["authority"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
                || list.summary["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();
    Ok(Json(
        json!({"ready":app.index.ready(&app.config).await,"usageReady":usage_ready,"total":lists.len(),"lists":lists.into_iter().take(100).map(|(rank,(id,list))|directory.summary(id,list,rank+1,12,&app.config.public_url)).collect::<Vec<_>>()}),
    ))
}
pub async fn list_profile(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
    protocol::pubkey(&id)?;
    let catalog = app.index.catalog(&app.config).await;
    let directory = &catalog.directory;
    let list = directory
        .lists
        .get(&id)
        .ok_or(Error(StatusCode::NOT_FOUND, "List unavailable"))?;
    let (_, usage) = usage::rankings(&app).await?;
    let rank = directory
        .ranked(&usage)
        .iter()
        .position(|(key, _)| **key == id)
        .unwrap_or(0)
        + 1;
    let mut stats = usage::stats(&app, format!("list:{id}")).await?;
    stats["ready"] = (stats["ready"] == true && app.index.ready(&app.config).await).into();
    directory.names(&mut stats);
    let mut summary = directory.summary(&id, list, rank, 4096, &app.config.public_url);
    stats["providers"] = summary["providers"].take();
    summary["providers"] = stats["providers"]
        .as_array()
        .unwrap()
        .iter()
        .take(12)
        .cloned()
        .collect();
    stats["list"] = summary;
    Ok(Json(stats))
}
pub async fn memberships(app: &App, wallet: &str) -> Result<Vec<Value>> {
    let wallet = protocol::pubkey(wallet)?;
    let catalog = app.index.catalog(&app.config).await;
    let (_, usage) = usage::rankings(app).await?;
    Ok(catalog
        .directory
        .ranked(&usage)
        .into_iter()
        .enumerate()
        .filter(|(_, (_, list))| list.contains(wallet.as_ref()))
        .map(|(rank, (id, list))| {
            catalog
                .directory
                .summary(id, list, rank + 1, 0, &app.config.public_url)
        })
        .collect())
}
pub async fn provider_profile(
    State(app): State<App>,
    Path(wallet): Path<String>,
) -> Result<Json<Value>> {
    protocol::pubkey(&wallet)?;
    let catalog = app.index.catalog(&app.config).await;
    let directory = &catalog.directory;
    let provider = directory
        .providers
        .get(&wallet)
        .ok_or(Error(StatusCode::NOT_FOUND, "Provider unavailable"))?;
    let mut stats = usage::stats(&app, format!("provider:{wallet}")).await?;
    stats["ready"] = (stats["ready"] == true && app.index.ready(&app.config).await).into();
    directory.names(&mut stats);
    stats["provider"] = provider.clone();
    stats["activity"] = usage::activity(&app, wallet.clone()).await?.into();
    stats["lists"] = memberships(&app, &wallet).await?.into();
    Ok(Json(stats))
}
pub async fn icon(State(app): State<App>, Path(id): Path<String>) -> Result<impl IntoResponse> {
    protocol::pubkey(&id)?;
    let catalog = app.index.catalog(&app.config).await;
    let uri = catalog
        .directory
        .lists
        .get(&id)
        .and_then(|l| l.summary["icon"].as_str())
        .ok_or(Error(StatusCode::NOT_FOUND, "Icon unavailable"))?;
    let raw = metadata::icon(&app, uri).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=300"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'none'; sandbox",
            ),
        ],
        Bytes::copy_from_slice(&raw),
    ))
}
