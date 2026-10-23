pub mod schema;
use crate::{
    App,
    db::Db,
    error::{Error, Result},
    protocol::{self, Kind},
    rpc,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Document {
    pub raw: Arc<[u8]>,
    pub value: Value,
}
pub fn reference(data: &[u8]) -> Option<(schema::Kind, &[u8], &str)> {
    let (kind, hash, len, uri) = match protocol::kind(data)? {
        Kind::Provider => (schema::Kind::Provider, 72, 104, 108),
        Kind::Metadata => (schema::Kind::Model, 104, 136, 140),
        _ => return None,
    };
    let (hash, uri) = protocol::metadata_uri(data, hash, len, uri)?;
    Some((kind, hash, uri))
}
pub fn key(data: &[u8]) -> Option<String> {
    let (kind, hash, _) = reference(data)?;
    Some(format!("{}:{}", kind.name(), hex::encode(hash)))
}
fn verified(kind: schema::Kind, hash: &[u8], raw: Vec<u8>) -> Option<Document> {
    if raw.len() > schema::MAX_BYTES || Sha256::digest(&raw).as_slice() != hash {
        return None;
    }
    let value = schema::parse(kind, &raw).ok()?;
    Some(Document {
        raw: raw.into(),
        value,
    })
}
pub async fn restore(db: &Db, max: usize, max_bytes: usize) -> Result<HashMap<String, Document>> {
    db.call(move |conn| {
        let mut statement =
            conn.prepare("SELECT hash,kind,body FROM documents WHERE length(body)<=?2 LIMIT ?1")?;
        let rows = statement.query_map([max, max_bytes.min(schema::MAX_BYTES)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut documents = HashMap::new();
        let mut bytes = 0usize;
        for row in rows {
            let (hash, kind, raw) = row?;
            if raw.len() > max_bytes.saturating_sub(bytes) {
                continue;
            }
            let kind = match kind.as_str() {
                "provider" => schema::Kind::Provider,
                "model" => schema::Kind::Model,
                _ => continue,
            };
            if let Ok(hash) = hex::decode(&hash)
                && let Some(doc) = verified(kind, &hash, raw)
            {
                bytes += doc.raw.len();
                documents.insert(format!("{}:{}", kind.name(), hex::encode(hash)), doc);
            }
        }
        Ok(documents)
    })
    .await
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || a == 169 && b == 254
                || a == 100 && (64..128).contains(&b)
                || a == 172 && (16..32).contains(&b)
                || a == 192 && (b == 168 || b == 0 || b == 88 && c == 99)
                || a == 198 && (b == 18 || b == 19 || b == 51 && c == 100)
                || a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (0x2000..0x4000).contains(&s[0])
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
        }
    }
}
async fn fetch(data: Arc<[u8]>) -> Option<Document> {
    let (kind, hash, uri) = reference(&data)?;
    let body = if let Some(encoded) = uri.strip_prefix("data:application/json;base64,") {
        STANDARD.decode(encoded).ok()?
    } else {
        let url = reqwest::Url::parse(uri).ok()?;
        let host = url.host_str()?.to_owned();
        if url.scheme() != "https"
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || host.parse::<IpAddr>().is_ok()
            || host.ends_with(".local")
            || host == "localhost"
        {
            return None;
        }
        let addresses: Vec<SocketAddr> = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host.as_str(), 443)),
        )
        .await
        .ok()?
        .ok()?
        .collect();
        if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
            return None;
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .resolve_to_addrs(&host, &addresses)
            .build()
            .ok()?;
        let response = client.get(url).send().await.ok()?.error_for_status().ok()?;
        rpc::limited(response, schema::MAX_BYTES).await.ok()?
    };
    verified(kind, hash, body)
}
pub async fn run(app: App) {
    let mut failures = HashMap::<String, Instant>::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let obsolete = {
            let mut state = app.index.state.write().await;
            let referenced: HashSet<_> = state
                .accounts
                .values()
                .filter_map(|record| key(&record.data))
                .collect();
            let obsolete = state
                .documents
                .keys()
                .filter(|key| !referenced.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            state.documents.retain(|key, _| referenced.contains(key));
            if !obsolete.is_empty() {
                state.cached = None;
            }
            obsolete
        };
        if !obsolete.is_empty() {
            let _ = app
                .db
                .call(move |conn| {
                    let tx = conn.transaction()?;
                    {
                        let mut statement =
                            tx.prepare_cached("DELETE FROM documents WHERE kind=?1 AND hash=?2")?;
                        for key in obsolete {
                            if let Some((kind, hash)) = key.split_once(':') {
                                statement.execute([kind, hash])?;
                            }
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await;
        }
        let requests = {
            let s = app.index.state.read().await;
            s.accounts
                .values()
                .filter_map(|r| {
                    let key = key(&r.data)?;
                    if s.documents.contains_key(&key)
                        || failures
                            .get(&key)
                            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
                    {
                        None
                    } else {
                        Some((key, r.data.clone()))
                    }
                })
                .take(32)
                .collect::<HashMap<_, _>>()
        };
        let mut work = stream::iter(requests)
            .map(|(key, data)| async move {
                let document = fetch(data).await;
                (key, document)
            })
            .buffer_unordered(4);
        while let Some((key, document)) = work.next().await {
            let Some(doc) = document else {
                failures.insert(key, Instant::now());
                continue;
            };
            {
                let state = app.index.state.read().await;
                if state.documents.values().map(|d| d.raw.len()).sum::<usize>() + doc.raw.len()
                    > app.config.max_rpc_bytes
                {
                    failures.insert(key, Instant::now());
                    continue;
                }
                if !state
                    .accounts
                    .values()
                    .any(|r| self::key(&r.data).as_deref() == Some(&key))
                {
                    continue;
                }
            }
            let Some((kind, hash)) = key.split_once(':') else {
                continue;
            };
            let (kind, hash, raw) = (kind.to_owned(), hash.to_owned(), doc.raw.to_vec());
            let saved = app
                .db
                .call(move |conn| {
                    conn.execute(
                        "INSERT OR REPLACE INTO documents VALUES(?1,?2,?3)",
                        rusqlite::params![hash, kind, raw],
                    )?;
                    Ok(())
                })
                .await;
            if saved.is_err() {
                continue;
            }
            let mut s = app.index.state.write().await;
            if s.documents.len() < app.config.max_accounts {
                s.documents.insert(key.clone(), doc);
                s.cached = None;
            }
            drop(s);
            failures.remove(&key);
            app.index.notify();
        }
        failures.retain(|_, at| at.elapsed() < Duration::from_secs(120));
    }
}
pub async fn document(app: &App, hash: &str) -> Result<Arc<[u8]>> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::bad("Invalid metadata hash"));
    }
    let s = app.index.state.read().await;
    for kind in ["provider", "model"] {
        if let Some(doc) = s.documents.get(&format!("{kind}:{hash}")) {
            return Ok(doc.raw.clone());
        }
    }
    Err(Error(
        axum::http::StatusCode::NOT_FOUND,
        "Metadata unavailable",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_private_and_special_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.168.1.1",
            "::1",
            "::ffff:127.0.0.1",
            "2002:7f00:1::",
            "2001:db8::1",
            "2001::1",
            "198.18.0.1",
        ] {
            assert!(!public_ip(value.parse().unwrap()), "{value}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700::1111".parse().unwrap()));
    }
    #[test]
    fn content_address_requires_exact_bytes_and_valid_schema() {
        let raw =
            br#"{"name":"Example","endpoint":"https://example.com/v1/chat/completions"}"#.to_vec();
        let hash = Sha256::digest(&raw);
        assert!(verified(schema::Kind::Provider, &hash, raw.clone()).is_some());
        assert!(verified(schema::Kind::Model, &hash, raw.clone()).is_none());
        assert!(verified(schema::Kind::Provider, &[0; 32], raw).is_none());
    }
}
