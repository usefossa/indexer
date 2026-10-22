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
pub fn decode(tx: &Value, program: &str, market: &str, signature: &str) -> Vec<Receipt> {
    if tx.get("meta").and_then(|v| v.get("err")) != Some(&Value::Null) {
        return Vec::new();
    }
    let Some(slot) = tx["slot"].as_u64() else {
        return Vec::new();
    };
    let keys: Vec<_> = tx["transaction"]["message"]["accountKeys"]
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
        .collect();
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
            if accounts.len() != 12 {
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
            if data.len() != 157 || data[0] != 7 {
                return None;
            }
            let r = &data[37..];
            let index = u64_at(&data, 29);
            let generation = u64_at(r, 96);
            let charge = u64_at(r, 104);
            if r[32..64] != *key(3)?
                || r[64..96] != *key(4)?
                || generation == 0
                || generation != u64_at(&data, 13)
                || charge == 0
                || index >= 65535
            {
                return None;
            }
            let deadline = u64_at(r, 112) as i64;
            Some(Receipt {
                bucket: bs58::encode(key(11)?).into_string(),
                epoch: u64_at(&data, 21) as i64,
                index,
                receipt: STANDARD.encode(r),
                model: bs58::encode(key(1)?).into_string(),
                provider: bs58::encode(key(5)?).into_string(),
                owner: address(r, 0),
                booking: address(r, 64),
                generation: generation.to_string(),
                charge: charge.to_string(),
                deadline,
                time: tx["blockTime"]
                    .as_i64()
                    .unwrap_or_else(|| deadline.saturating_sub(3900)),
                tokens: [1, 5, 9]
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
async fn process(app: &App) -> Result<()> {
    let pending = app
        .db
        .call(|conn| {
            let mut statement = conn.prepare(
                "SELECT signature FROM signatures WHERE done=0 AND next_attempt<=?1 ORDER BY next_attempt,slot DESC LIMIT 32",
            )?;
            Ok(statement
                .query_map([crate::now()], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await?;
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
        let receipts = decode(
            &tx,
            &app.config.program.to_string(),
            &app.config.market.to_string(),
            &signature,
        );
        app.db
            .call(move |conn| {
                let transaction = conn.transaction()?;
                for receipt in receipts {
                    let id = format!("{}:{}:{}", receipt.bucket, receipt.epoch, receipt.index);
                    transaction.execute(
                        "INSERT OR IGNORE INTO receipts VALUES(?1,?2,?3,?4,?5)",
                        params![
                            id,
                            receipt.owner,
                            receipt.provider,
                            receipt.slot,
                            serde_json::to_string(&receipt)?
                        ],
                    )?;
                }
                transaction.execute(
                    "UPDATE signatures SET done=1 WHERE signature=?1",
                    [signature],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await?;
    }
    Ok(())
}
async fn retry(app: &App, signature: String) -> Result<()> {
    app.db.call(move|conn|{conn.execute("UPDATE signatures SET next_attempt=?1+min(3600,5*(1<<min(attempts,10))),attempts=attempts+1 WHERE signature=?2",params![crate::now(),signature])?;Ok(())}).await
}
pub async fn run(app: App) {
    let mut timer = tokio::time::interval(Duration::from_secs(3));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        if !app.index.ready(&app.config).await {
            continue;
        }
        for historical in [false, true] {
            if let Err(error) = discover(&app, historical).await {
                tracing::warn!(%error,"receipt discovery unavailable");
            }
        }
        if let Err(error) = process(&app).await {
            tracing::warn!(%error,"receipt persistence unavailable");
        }
    }
}
pub async fn history(app: &App, wallet: &str, provider: bool) -> Result<Value> {
    let wallet = wallet.to_owned();
    app.db.call(move|conn|{
        let sql=if provider{"SELECT body FROM receipts WHERE provider=?1 ORDER BY slot DESC LIMIT 1000"}else{"SELECT body FROM receipts WHERE owner=?1 ORDER BY slot DESC LIMIT 1000"};
        let mut statement=conn.prepare(sql)?;let rows=statement.query_map([wallet],|r|r.get::<_,String>(0))?;let mut receipts=Vec::new();for row in rows{receipts.push(serde_json::from_str::<Value>(&row?)?);}
        let pending:u64=conn.query_row("SELECT count(*) FROM signatures WHERE done=0",[],|r|r.get(0))?;
        let exhausted:Option<String>=conn.query_row("SELECT value FROM settings WHERE key='history_exhausted'",[],|r|r.get(0)).optional()?;
        let checked:Option<String>=conn.query_row("SELECT value FROM settings WHERE key='receipts_checked_at'",[],|r|r.get(0)).optional()?;
        Ok(json!({"receipts":receipts,"coverage":{"complete":false,"commitment":"finalized","rpcHistoryExhausted":exhausted.as_deref()==Some("true"),"pendingTransactions":pending,"lastChecked":checked.and_then(|s|s.parse::<u64>().ok()),"limit":1000,"note":"History includes successfully retrieved finalized transactions. RPC retention may omit older transactions; completeness is not guaranteed."}}))
    }).await
}
