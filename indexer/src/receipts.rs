use crate::{
    App,
    error::{Error, Result},
    protocol::{address, u32_at, u64_at},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    #[serde(default = "legacy_version")]
    pub version: u8,
    pub bucket: String,
    pub epoch: i64,
    pub index: u64,
    pub receipt: String,
    pub model: String,
    pub provider: String,
    pub owner: String,
    pub booking: String,
    pub generation: String,
    pub charge: String,
    pub deadline: i64,
    pub time: i64,
    pub tokens: u64,
    pub signature: String,
    pub slot: u64,
}
impl Receipt {
    pub fn lane(&self) -> Option<String> {
        let raw = STANDARD.decode(&self.receipt).ok()?;
        (raw.len() == 120).then(|| address(&raw, 32))
    }
    pub fn storage_id(&self) -> Option<String> {
        let id = format!("{}:{}:{}", self.bucket, self.epoch, self.index);
        Some(if self.version == 2 {
            format!("{id}:{}", self.lane()?)
        } else {
            id
        })
    }
}
fn legacy_version() -> u8 {
    1
}
pub(crate) fn transaction_keys(tx: &Value) -> Vec<Vec<u8>> {
    tx["transaction"]["message"]["accountKeys"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            tx["meta"]["loadedAddresses"]["writable"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .chain(
            tx["meta"]["loadedAddresses"]["readonly"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .map(|v| {
            v.as_str()
                .and_then(|s| bs58::decode(s).into_vec().ok())
                .filter(|k| k.len() == 32)
                .unwrap_or_default()
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct BookingList {
    pub booking: String,
    pub generation: String,
    pub list: String,
    pub lane: String,
    pub owner: String,
    pub model: String,
    pub slot: u64,
}
pub fn decode_bookings(tx: &Value, program: &str, market: &str) -> Vec<BookingList> {
    if tx.get("meta").and_then(|m| m.get("err")) != Some(&Value::Null) {
        return Vec::new();
    }
    let Some(slot) = tx["slot"].as_u64().filter(|s| *s <= i64::MAX as u64) else {
        return Vec::new();
    };
    let keys = transaction_keys(tx);
    tx["transaction"]["message"]["instructions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|ix| {
            if bs58::encode(keys.get(usize::try_from(ix["programIdIndex"].as_u64()?).ok()?)?)
                .into_string()
                != program
            {
                return None;
            }
            let accounts = ix["accounts"]
                .as_array()
                .filter(|a| [9, 10].contains(&a.len()))?;
            let key = |i: usize| {
                keys.get(usize::try_from(accounts.get(i)?.as_u64()?).ok()?)
                    .filter(|k| k.len() == 32)
                    .map(|k| bs58::encode(k).into_string())
            };
            if key(0)? != market {
                return None;
            }
            let data = bs58::decode(ix["data"].as_str()?).into_vec().ok()?;
            let native = data.starts_with(&crate::protocol::RESERVE_BOOKING_DISCRIMINATOR);
            let generation = if native && accounts.len() == 10 {
                match (data.len(), data.get(154)) {
                    (155, Some(0)) => 0,
                    (163, Some(1)) if u64_at(&data, 155) > 0 => u64_at(&data, 155),
                    (116, _) if data[115] == 0 => 0,
                    (124, _) if data[115] == 1 && u64_at(&data, 116) > 0 => u64_at(&data, 116),
                    _ => return None,
                }
            } else if accounts.len() == 9
                && data.first() == Some(&6)
                && [108, 116].contains(&data.len())
            {
                let generation = if data.len() == 116 {
                    u64_at(&data, 108)
                } else {
                    0
                };
                if generation >= 1 << 56 {
                    return None;
                }
                generation
            } else {
                return None;
            };
            Some(BookingList {
                booking: key(6)?,
                generation: generation.to_string(),
                list: key(8)?,
                lane: key(4)?,
                owner: key(5)?,
                model: key(1)?,
                slot,
            })
        })
        .collect()
}

pub fn decode(tx: &Value, program: &str, market: &str, signature: &str) -> Vec<Receipt> {
    if tx.get("meta").and_then(|v| v.get("err")) != Some(&Value::Null) {
        return Vec::new();
    }
    let Some(slot) = tx["slot"].as_u64() else {
        return Vec::new();
    };
    let keys = transaction_keys(tx);
    tx["transaction"]["message"]["instructions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|ix| {
            let program_index = usize::try_from(ix["programIdIndex"].as_u64()?).ok()?;
            if bs58::encode(keys.get(program_index)?).into_string() != program {
                return None;
            }
            let accounts = ix["accounts"].as_array()?;
            if ![11, 12, 13].contains(&accounts.len()) {
                return None;
            }
            let key = |i: usize| {
                keys.get(usize::try_from(accounts.get(i)?.as_u64()?).ok()?)
                    .filter(|k| k.len() == 32)
            };
            if bs58::encode(key(0)?).into_string() != market {
                return None;
            }
            let data = bs58::decode(ix["data"].as_str()?).into_vec().ok()?;
            let native = data.starts_with(&crate::protocol::CLAIM_BOOKING_DISCRIMINATOR);
            let version = if native
                && data.len() == 141
                && data[20] == 1
                && [11, 13].contains(&accounts.len())
            {
                2
            } else {
                match (data.len(), accounts.len(), data.first()) {
                    (141, 11, Some(7)) => 2,
                    (157, 12, Some(7)) => 1,
                    _ => return None,
                }
            };
            let r = &data[if version == 2 { 21 } else { 37 }..];
            let index = if version == 2 {
                u64_at(r, 96)
            } else {
                u64_at(&data, 29)
            };
            let generation = u64_at(r, 96);
            let charge = u64_at(r, 104);
            if r[32..64] != *key(3)?
                || r[64..96] != *key(4)?
                || generation == 0
                || !native && (generation != u64_at(&data, 13) || generation >= 1 << 56)
                || version == 1 && (charge == 0 || index >= 65535)
            {
                return None;
            }
            let deadline = u64_at(r, 112) as i64;
            Some(Receipt {
                version,
                bucket: bs58::encode(key(if version == 2 { 4 } else { 11 })?).into_string(),
                epoch: if version == 2 {
                    0
                } else {
                    u64_at(&data, 21) as i64
                },
                index,
                receipt: STANDARD.encode(r),
                model: bs58::encode(key(1)?).into_string(),
                provider: bs58::encode(key(5)?).into_string(),
                owner: address(r, 0),
                booking: address(r, 64),
                generation: generation.to_string(),
                charge: charge.to_string(),
                deadline,
                time: tx["blockTime"].as_i64().unwrap_or_else(|| {
                    deadline.saturating_sub(if version == 2 { 300 } else { 3900 })
                }),
                tokens: if native { [8, 12, 16] } else { [1, 5, 9] }
                    .into_iter()
                    .map(|at| u32_at(&data, at) as u64)
                    .sum(),
                signature: signature.into(),
                slot,
            })
        })
        .collect()
}
async fn discover(app: &App, historical: bool) -> Result<()> {
    let prefix = if historical { "history" } else { "recent" };
    if historical && app.db.get("history_exhausted").await?.as_deref() == Some("true") {
        return Ok(());
    }
    let cursor = app.db.get(&format!("{prefix}_before")).await?;
    let head = app.db.get("receipt_head").await?;
    let initial = !historical && head.is_none();
    if historical && head.is_none() {
        return Ok(());
    }
    let before = cursor.or_else(|| if historical { head.clone() } else { None });
    let mut options = json!({"commitment":"finalized","limit":100});
    if let Some(before) = before {
        options["before"] = before.into();
    }
    if !historical && let Some(head) = head {
        options["until"] = head.into();
    }
    let value = app
        .rpc
        .call(
            "getSignaturesForAddress",
            json!([app.config.program.to_string(), options]),
        )
        .await?;
    let rows = value
        .as_array()
        .filter(|v| v.len() <= 100)
        .ok_or(Error::unavailable("Invalid signature history"))?;
    let mut signatures = Vec::new();
    for row in rows {
        let signature = row["signature"]
            .as_str()
            .filter(|s| bs58::decode(s).into_vec().is_ok_and(|v| v.len() == 64))
            .ok_or(Error::unavailable("Invalid history signature"))?;
        let slot = row["slot"]
            .as_u64()
            .filter(|s| *s <= i64::MAX as u64)
            .ok_or(Error::unavailable("Invalid history slot"))?;
        if row["confirmationStatus"]
            .as_str()
            .is_some_and(|s| s != "finalized")
        {
            return Err(Error::unavailable("Receipt history is not finalized"));
        }
        signatures.push((signature.to_owned(), slot, !row["err"].is_null()));
    }
    let first = signatures.first().map(|r| r.0.clone());
    let last = signatures.last().map(|r| r.0.clone());
    let exhausted = signatures.len() < 100;
    let done = exhausted || initial;
    let prefix = prefix.to_owned();
    app.db.call(move|conn|{
        let tx=conn.transaction()?;
        {let mut statement=tx.prepare_cached("INSERT OR IGNORE INTO signatures(signature,slot,done) VALUES(?1,?2,?3)")?;for (signature,slot,failed) in signatures {statement.execute(params![signature,slot,if failed{1}else{0}])?;}}
        if !historical&& let Some(first)=first {tx.execute("INSERT OR IGNORE INTO settings VALUES('recent_head',?1)",[first])?;}
        if initial {
            if let Some(last)=&last {tx.execute("INSERT OR REPLACE INTO settings VALUES('history_before',?1)",[last])?;}
            if exhausted {tx.execute("INSERT OR REPLACE INTO settings VALUES('history_exhausted','true')",[])?;}
        }
        if done {
            tx.execute("DELETE FROM settings WHERE key=?1",[format!("{prefix}_before")])?;
            if historical {tx.execute("INSERT OR REPLACE INTO settings VALUES('history_exhausted','true')",[])?;}
            else {tx.execute("INSERT OR REPLACE INTO settings SELECT 'receipt_head',value FROM settings WHERE key='recent_head'",[])?;tx.execute("DELETE FROM settings WHERE key='recent_head'",[])?;}
        }else if let Some(last)=last {tx.execute("INSERT OR REPLACE INTO settings VALUES(?1,?2)",params![format!("{prefix}_before"),last])?;}
        tx.execute("INSERT OR REPLACE INTO settings VALUES('receipts_checked_at',?1)",[crate::now().to_string()])?;
        tx.commit()?;Ok(())
    }).await
}
async fn process(app: &App, turn: u8, blocks: &mut crate::one_shot::Blocks) -> Result<usize> {
    let pending = app
        .db
        .call(move |conn| {
            let select = |retry: bool, newest: bool, limit: usize| -> Result<Vec<String>> {
                let (filter, order) = if retry { ("attempts>0", "next_attempt,slot") }
                    else { ("attempts=0", if newest { "slot DESC" } else { "slot" }) };
                let mut statement = conn.prepare(&format!("SELECT signature FROM signatures WHERE done=0 AND next_attempt<=?1 AND {filter} ORDER BY {order} LIMIT ?2"))?;
                Ok(statement.query_map([crate::now(), limit as u64], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?)
            };
            let mut pending = select(turn == 1, turn == 0, 32)?;
            if pending.len() < 32 { pending.extend(select(turn != 1, false, 32 - pending.len())?); }
            Ok(pending)
        })
        .await?;
    let count = pending.len();
    let mut work=stream::iter(pending).map(|signature|{let app=app.clone();async move{let result=app.rpc.call("getTransaction",json!([signature,{"encoding":"json","commitment":"finalized","maxSupportedTransactionVersion":0}])).await; (signature,result)}}).buffer_unordered(4);
    while let Some((signature, result)) = work.next().await {
        let tx = match result {
            Ok(value) if !value.is_null() => value,
            _ => {
                retry(app, signature).await?;
                continue;
            }
        };
        if tx.get("meta").is_none_or(Value::is_null) || tx["transaction"]["message"].is_null() {
            retry(app, signature).await?;
            continue;
        }
        let events = match crate::one_shot::decode(
            &tx,
            &app.config.program.to_string(),
            &app.config.market.to_string(),
            &app.config.mint.to_string(),
        ) {
            Ok(events) => events,
            Err(_) => {
                retry(app, signature).await?;
                continue;
            }
        };
        let slot = tx["slot"]
            .as_u64()
            .filter(|s| *s <= i64::MAX as u64)
            .ok_or(Error::unavailable("Invalid transaction slot"))?;
        let order = if events.is_empty() {
            0
        } else {
            match crate::one_shot::order(app, slot, &signature, blocks).await {
                Ok(order) => order,
                Err(_) => {
                    retry(app, signature).await?;
                    continue;
                }
            }
        };
        let time = tx["blockTime"].as_i64().unwrap_or(0);
        let receipts = decode(
            &tx,
            &app.config.program.to_string(),
            &app.config.market.to_string(),
            &signature,
        );
        let bookings = decode_bookings(
            &tx,
            &app.config.program.to_string(),
            &app.config.market.to_string(),
        );
        app.db
            .call(move |conn| {
                let transaction = conn.transaction()?;
                crate::usage::store(&transaction, &bookings, &receipts)?;
                crate::one_shot::store(&transaction, &signature, slot, order, time, &events)?;
                transaction.execute(
                    "UPDATE signatures SET done=1 WHERE signature=?1",
                    [signature],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await?;
    }
    Ok(count)
}
async fn retry(app: &App, signature: String) -> Result<()> {
    app.db.call(move|conn|{conn.execute("UPDATE signatures SET next_attempt=?1+min(3600,5*(1<<min(attempts,10))),attempts=attempts+1 WHERE signature=?2",params![crate::now(),signature])?;Ok(())}).await
}
pub async fn run(app: App) {
    tokio::join!(discovery(&app), drain(&app));
}
async fn discovery(app: &App) {
    let mut timer = tokio::time::interval(Duration::from_secs(3));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        if !app.index.ready(&app.config).await {
            continue;
        }
        for historical in [false, true] {
            if let Err(error) = discover(app, historical).await {
                tracing::warn!(%error,"receipt discovery unavailable");
            }
        }
    }
}
async fn drain(app: &App) {
    let mut turn = 0;
    let mut blocks = crate::one_shot::Blocks::new();
    loop {
        let delay = if !app.index.ready(&app.config).await {
            Duration::from_secs(1)
        } else {
            match process(app, turn, &mut blocks).await {
                Ok(count) => {
                    if let Err(error) = crate::one_shot::resolve(app).await {
                        tracing::warn!(%error,"one-shot usage attribution unavailable");
                    }
                    turn = (turn + 1) % 3;
                    if count == 32 {
                        Duration::from_millis(10)
                    } else {
                        Duration::from_millis(500)
                    }
                }
                Err(error) => {
                    tracing::warn!(%error,"receipt persistence unavailable");
                    Duration::from_secs(1)
                }
            }
        };
        tokio::time::sleep(delay).await;
    }
}
pub async fn history(app: &App, wallet: &str, provider: bool) -> Result<Value> {
    let wallet = wallet.to_owned();
    app.db.call(move|conn|{
        let sql=if provider{"SELECT body FROM receipts WHERE provider=?1 ORDER BY slot DESC LIMIT 1000"}else{"SELECT body FROM receipts WHERE owner=?1 ORDER BY slot DESC LIMIT 1000"};
        let mut statement=conn.prepare(sql)?;let rows=statement.query_map([wallet],|r|r.get::<_,String>(0))?;let mut receipts=Vec::new();for row in rows{receipts.push(serde_json::to_value(serde_json::from_str::<Receipt>(&row?)?)?);}
        let pending:u64=conn.query_row("SELECT count(*) FROM signatures WHERE done=0",[],|r|r.get(0))?;
        let exhausted:Option<String>=conn.query_row("SELECT value FROM settings WHERE key='history_exhausted'",[],|r|r.get(0)).optional()?;
        let checked:Option<String>=conn.query_row("SELECT value FROM settings WHERE key='receipts_checked_at'",[],|r|r.get(0)).optional()?;
        Ok(json!({"receipts":receipts,"coverage":{"complete":false,"commitment":"finalized","rpcHistoryExhausted":exhausted.as_deref()==Some("true"),"pendingTransactions":pending,"lastChecked":checked.and_then(|s|s.parse::<u64>().ok()),"limit":1000,"note":"History includes successfully retrieved finalized transactions. RPC retention may omit older transactions; completeness is not guaranteed."}}))
    }).await
}
