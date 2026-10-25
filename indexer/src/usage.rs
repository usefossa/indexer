use crate::{
    App,
    error::{Error, Result},
    receipts::{BookingList, Receipt},
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(crate) fn add(
    tx: &Transaction<'_>,
    scope: &str,
    model: &str,
    provider: &str,
    tokens: u64,
    charge: &str,
) -> Result<()> {
    let current = tx
        .query_row(
            "SELECT requests,tokens,spent FROM usage_stats WHERE scope=?1",
            [scope],
            |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let (requests, old_tokens, spent) = current.unwrap_or((0, "0".into(), "0".into()));
    let tokens = old_tokens
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_add(tokens.into()))
        .ok_or(Error::internal())?;
    let spent = spent
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_add(charge.parse().ok()?))
        .ok_or(Error::internal())?;
    let requests = requests
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or(Error::internal())?;
    tx.execute(
        "INSERT OR REPLACE INTO usage_stats VALUES(?1,?2,?3,?4)",
        params![scope, requests, tokens.to_string(), spent.to_string()],
    )?;
    for (kind, key) in [("model", model), ("provider", provider)] {
        tx.execute("INSERT INTO usage_groups VALUES(?1,?2,?3,1) ON CONFLICT(scope,kind,key) DO UPDATE SET requests=requests+1", params![scope,kind,key])?;
    }
    Ok(())
}

pub fn store(tx: &Transaction<'_>, bookings: &[BookingList], receipts: &[Receipt]) -> Result<()> {
    for booking in bookings {
        tx.execute(
            "INSERT OR IGNORE INTO booking_lists VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                booking.booking,
                booking.generation,
                booking.lane,
                booking.list,
                booking.owner,
                booking.model,
                booking.slot
            ],
        )?;
        let mut query = tx.prepare_cached("SELECT u.id,r.body FROM receipt_usage u JOIN receipts r ON r.id=u.id JOIN booking_lists b ON b.booking=u.booking AND b.generation=u.generation AND b.lane=u.lane AND b.owner=u.owner AND b.model=u.model AND b.slot<=r.slot WHERE u.booking=?1 AND u.generation=?2 AND u.lane=?3 AND u.list IS NULL")?;
        let rows = query
            .query_map(
                params![booking.booking, booking.generation, booking.lane],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, body) in rows {
            let receipt: Receipt = serde_json::from_str(&body)?;
            // select the persisted binding: replayed input cannot replace its original list.
            let list: String = tx.query_row(
                "SELECT list FROM booking_lists WHERE booking=?1 AND generation=?2 AND lane=?3",
                params![booking.booking, booking.generation, booking.lane],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE receipt_usage SET list=?1 WHERE id=?2",
                params![list, id],
            )?;
            add(
                tx,
                &format!("list:{list}"),
                &receipt.model,
                &receipt.provider,
                receipt.tokens,
                &receipt.charge,
            )?;
        }
    }
    for receipt in receipts {
        let id = receipt.storage_id().ok_or(Error::internal())?;
        let lane = receipt.lane().ok_or(Error::internal())?;
        tx.execute(
            "INSERT OR IGNORE INTO receipts VALUES(?1,?2,?3,?4,?5)",
            params![
                id,
                receipt.owner,
                receipt.provider,
                receipt.slot,
                serde_json::to_string(receipt)?
            ],
        )?;
        // replace a pre-upgrade storage key only when it identifies this same settlement.
        if receipt.version == 2 {
            let old = format!("{}:{}:{}", receipt.bucket, receipt.epoch, receipt.index);
            tx.execute(
                "DELETE FROM receipts WHERE id=?1 AND json_extract(body,'$.signature')=?2 AND json_extract(body,'$.receipt')=?3",
                params![old, receipt.signature, receipt.receipt],
            )?;
        }
        let list: Option<String> = tx.query_row("SELECT list FROM booking_lists WHERE booking=?1 AND generation=?2 AND owner=?3 AND model=?4 AND slot<=?5 AND lane=?6",params![receipt.booking,receipt.generation,receipt.owner,receipt.model,receipt.slot,lane],|r|r.get(0)).optional()?;
        if tx.execute(
            "INSERT OR IGNORE INTO receipt_usage VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                receipt.booking,
                receipt.generation,
                lane,
                receipt.owner,
                receipt.model,
                list
            ],
        )? == 0
        {
            continue;
        }
        add(
            tx,
            &format!("provider:{}", receipt.provider),
            &receipt.model,
            &receipt.provider,
            receipt.tokens,
            &receipt.charge,
        )?;
        if let Some(list) = list {
            add(
                tx,
                &format!("list:{list}"),
                &receipt.model,
                &receipt.provider,
                receipt.tokens,
                &receipt.charge,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn ready(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key='history_exhausted' AND value='true') AND EXISTS(SELECT 1 FROM settings WHERE key='receipts_checked_at') AND NOT EXISTS(SELECT 1 FROM signatures WHERE done=0) AND NOT EXISTS(SELECT 1 FROM settings WHERE key='recent_before')",[],|r|r.get(0))
}
fn usage_ready(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    Ok(ready(conn)?
        && !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM one_shot_claims WHERE assigned=0)",
            [],
            |r| r.get::<_, bool>(0),
        )?)
}
pub async fn rankings(app: &App) -> Result<(bool, HashMap<String, u64>)> {
    app.db
        .call(|conn| {
            let mut query = conn.prepare_cached(
                "SELECT substr(scope,6),requests FROM usage_stats WHERE scope GLOB 'list:*'",
            )?;
            let counts = query
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            Ok((usage_ready(conn)?, counts))
        })
        .await
}
pub async fn stats(app: &App, scope: String) -> Result<Value> {
    app.db.call(move|conn| {
        let totals=conn.query_row("SELECT requests,tokens,spent FROM usage_stats WHERE scope=?1",[&scope],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?.unwrap_or((0,"0".into(),"0".into()));
        let top=|kind:&str| -> Result<Option<String>> { Ok(conn.query_row("SELECT key FROM usage_groups WHERE scope=?1 AND kind=?2 ORDER BY requests DESC,key LIMIT 1",params![scope,kind],|r|r.get(0)).optional()?) };
        let tokens=totals.1.parse::<u64>().map_err(|_|Error::unavailable("Usage counter exceeds JSON integer range"))?;
        Ok(json!({"ready":usage_ready(conn)?,"requests":totals.0,"tokens":tokens,"spentAtomic":totals.2,"topModel":top("model")?,"topProvider":top("provider")?}))
    }).await
}

pub async fn activity(app: &App, wallet: String) -> Result<Vec<Value>> {
    app.db.call(move |conn| {
        let mut query=conn.prepare_cached("SELECT time,tokens FROM (SELECT json_extract(body,'$.time') AS time,json_extract(body,'$.tokens') AS tokens,slot FROM receipts WHERE provider=?1 UNION ALL SELECT time,CAST(tokens AS INTEGER),slot FROM one_shot_claims WHERE provider=?1 AND assigned=1) ORDER BY slot DESC LIMIT 1000")?;
        let rows=query.query_map([wallet],|r|Ok((r.get::<_,Option<i64>>(0)?,r.get::<_,Option<u64>>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(rows.into_iter().filter_map(|(time,tokens)|Some(json!({"time":time?,"tokens":tokens?}))).collect())
    }).await
}
