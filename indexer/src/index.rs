use crate::{
    catalog,
    config::Config,
    db::Db,
    error::{Error, Result},
    metadata::Document,
    protocol::{self, Kind},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};
use tokio::sync::{Mutex, RwLock, watch};

#[derive(Clone, Debug)]
pub struct Record {
    pub slot: u64,
    pub version: u64,
    pub owner: String,
    pub lamports: u64,
    pub data: Arc<[u8]>,
}
impl Record {
    pub fn closed(slot: u64, version: u64) -> Self {
        Self {
            slot,
            version,
            owner: String::new(),
            lamports: 0,
            data: Arc::from([]),
        }
    }
    pub fn from_rpc(value: &Value, slot: u64, version: u64) -> Result<Self> {
        if value.is_null() || value["lamports"] == 0 {
            return Ok(Self::closed(slot, version));
        }
        Ok(Self {
            slot,
            version,
            owner: value["owner"]
                .as_str()
                .ok_or(Error::unavailable("Invalid account owner"))?
                .into(),
            lamports: value["lamports"]
                .as_u64()
                .ok_or(Error::unavailable("Invalid account balance"))?,
            data: protocol::bytes(value)
                .ok_or(Error::unavailable("Invalid account encoding"))?
                .into(),
        })
    }
    pub fn envelope(&self) -> Value {
        json!({"owner":if self.data.is_empty(){None}else{Some(&self.owner)},"data":if self.data.is_empty(){None}else{Some(STANDARD.encode(&self.data))},"slot":self.slot,"version":self.version})
    }
}
pub struct State {
    pub accounts: HashMap<String, Record>,
    pub documents: HashMap<String, Document>,
    pub slot: u64,
    pub floor: u64,
    pub connected: bool,
    pub reconciled: Option<Instant>,
    pub activity: Option<Instant>,
    pub cached: Option<Arc<catalog::Catalog>>,
    pub bytes: usize,
}
impl State {
    pub fn ready(&self, config: &Config) -> bool {
        self.connected
            && self
                .reconciled
                .is_some_and(|at| at.elapsed() < config.stale)
            && self.activity.is_some_and(|at| at.elapsed() < config.stale)
            && self
                .accounts
                .get(&config.market.to_string())
                .is_some_and(|r| {
                    protocol::validate(&config.market.to_string(), &r.data, config)
                        == Some(Kind::Market)
                })
    }
}
#[derive(Clone)]
pub struct Index {
    pub state: Arc<RwLock<State>>,
    pub changes: watch::Sender<u64>,
    writer: Arc<Mutex<()>>,
}
impl Index {
    pub async fn restore(db: &Db, config: &Config) -> Result<Self> {
        let config_ = config.clone();
        let accounts = db
            .call(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT address,slot,version,owner,data,lamports FROM accounts LIMIT ?1",
                )?;
                let rows = statement.query_map([config_.max_accounts + 1], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Record {
                            slot: r.get(1)?,
                            version: r.get::<_, String>(2)?.parse().unwrap_or(0),
                            owner: r.get(3)?,
                            data: r.get::<_, Vec<u8>>(4)?.into(),
                            lamports: r.get::<_, String>(5)?.parse().unwrap_or(0),
                        },
                    ))
                })?;
                let mut accounts = HashMap::new();
                for row in rows {
                    let (address, record) = row?;
                    if record.data.is_empty()
                        || record.owner == config_.program.to_string()
                            && protocol::validate(&address, &record.data, &config_).is_some()
                    {
                        accounts.insert(address, record);
                    }
                }
                if accounts.len() > config_.max_accounts {
                    return Err(Error::bad("Stored accounts exceed configured limit"));
                }
                Ok(accounts)
            })
            .await?;
        let floor = db
            .get("floor")
            .await?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let documents = crate::metadata::restore(db, config.max_accounts).await?;
        let slot = accounts.values().map(|r| r.slot).max().unwrap_or(floor);
        let bytes = accounts.values().map(|r| r.data.len()).sum();
        if bytes > config.max_rpc_bytes {
            return Err(Error::bad("Stored account bytes exceed configured limit"));
        }
        Ok(Self {
            state: Arc::new(RwLock::new(State {
                accounts,
                documents,
                slot,
                floor,
                connected: false,
                reconciled: None,
                activity: None,
                cached: None,
                bytes,
            })),
            changes: watch::channel(0).0,
            writer: Arc::new(Mutex::new(())),
        })
    }
    pub fn notify(&self) {
        self.changes.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub async fn connected(&self, value: bool) {
        let mut s = self.state.write().await;
        s.connected = value;
        s.cached = None;
        if !value {
            s.reconciled = None;
            s.activity = None;
        }
        drop(s);
        self.notify();
    }
    pub async fn activity(&self) {
        self.state.write().await.activity = Some(Instant::now());
    }
    pub async fn catalog(&self, config: &Config) -> Arc<catalog::Catalog> {
        {
            let s = self.state.read().await;
            if let Some(c) = &s.cached
                && c.ready == s.ready(config)
            {
                return c.clone();
            }
        }
        let mut s = self.state.write().await;
        let value = Arc::new(catalog::build(&s, config));
        s.cached = Some(value.clone());
        value
    }
    pub async fn ready(&self, config: &Config) -> bool {
        self.state.read().await.ready(config)
    }
    pub async fn apply(
        &self,
        db: &Db,
        config: &Config,
        updates: Vec<(String, Record)>,
        snapshot: Option<u64>,
    ) -> Result<()> {
        let _writer = self.writer.lock().await;
        let s = self.state.read().await;
        if snapshot.is_some_and(|slot| slot < s.floor) {
            return Err(Error::unavailable("RPC snapshot regressed"));
        }
        let mut changes = HashMap::new();
        let mut present = HashSet::new();
        let mut invalidate = false;
        for (address, mut record) in updates {
            if record.slot > i64::MAX as u64 {
                return Err(Error::unavailable("Invalid account slot"));
            }
            if snapshot.is_none() && record.slot <= s.floor {
                continue;
            }
            if record.owner != config.program.to_string()
                || protocol::validate(&address, &record.data, config).is_none()
            {
                record = Record::closed(record.slot, record.version);
            }
            if snapshot.is_some() && !record.data.is_empty() {
                present.insert(address.clone());
            }
            let old = changes.get(&address).or_else(|| s.accounts.get(&address));
            if old.is_some_and(|old: &Record| {
                old.slot > record.slot
                    || snapshot.is_none()
                        && old.slot == record.slot
                        && old.version >= record.version
            }) {
                continue;
            }
            if record.data.is_empty() && old.is_none() {
                continue;
            }
            invalidate |= !matches!(
                protocol::kind(&record.data).or_else(|| old.and_then(|r| protocol::kind(&r.data))),
                Some(Kind::Booking | Kind::User | Kind::Unbond | Kind::Receipt)
            );
            changes.insert(address, record);
        }
        if let Some(slot) = snapshot {
            for (address, record) in &s.accounts {
                if record.slot <= slot && !present.contains(address) {
                    changes.insert(address.clone(), Record::closed(slot, u64::MAX));
                }
            }
            let market = config.market.to_string();
            let record = changes.get(&market).or_else(|| s.accounts.get(&market));
            if record
                .is_none_or(|r| protocol::validate(&market, &r.data, config) != Some(Kind::Market))
            {
                return Err(Error::unavailable(
                    "Canonical market account missing or mint mismatched",
                ));
            }
            invalidate = true;
        }
        let floor = snapshot.unwrap_or(s.floor);
        let mut count = s.accounts.len();
        let mut bytes = s.bytes;
        for (address, record) in &changes {
            if let Some(old) = s.accounts.get(address) {
                bytes -= old.data.len();
            } else {
                count += 1;
            }
            bytes = bytes.saturating_add(record.data.len());
            if record.data.is_empty() && record.slot <= floor {
                count -= 1;
            }
        }
        if count > config.max_accounts || bytes > config.max_rpc_bytes {
            return Err(Error::unavailable("Account index capacity exceeded"));
        }
        let records: Vec<_> = changes.into_iter().collect();
        drop(s);
        db.accounts(records.clone(), snapshot).await?;
        let mut s = self.state.write().await;
        s.bytes = bytes;
        for (address, record) in records {
            s.slot = s.slot.max(record.slot);
            s.accounts.insert(address, record);
        }
        if let Some(slot) = snapshot {
            s.accounts
                .retain(|_, r| !r.data.is_empty() || r.slot > slot);
            s.floor = slot;
            s.slot = s.slot.max(slot);
            s.reconciled = Some(Instant::now());
        }
        if invalidate {
            s.cached = None;
        }
        drop(s);
        self.notify();
        Ok(())
    }
}
