use crate::{
    App,
    error::{Error, Result},
    protocol::{u32_at, u64_at},
    receipts::transaction_keys,
    usage,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashMap;

pub type Blocks = HashMap<u64, HashMap<String, u32>>;
pub struct Event {
    ix: usize,
    accounts: Vec<String>,
    data: Vec<u8>,
    charge: Option<u64>,
}
pub fn decode(tx: &Value, program: &str, market: &str, mint: &str) -> Result<Vec<Event>> {
    if tx.get("meta").and_then(|m| m.get("err")) != Some(&Value::Null) {
        return Ok(vec![]);
    }
    let keys = transaction_keys(tx)
        .into_iter()
        .map(|k| {
            if k.len() == 32 {
                bs58::encode(k).into_string()
            } else {
                String::new()
            }
        })
        .collect::<Vec<_>>();
    let mut events = vec![];
    for (index, ix) in tx["transaction"]["message"]["instructions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if ix["programIdIndex"]
            .as_u64()
            .and_then(|i| keys.get(i as usize))
            .is_none_or(|key| key != program)
        {
            continue;
        }
        let Some(accounts) = ix["accounts"].as_array().and_then(|a| {
            a.iter()
                .map(|i| {
                    keys.get(i.as_u64()? as usize)
                        .filter(|k| !k.is_empty())
                        .cloned()
                })
                .collect::<Option<Vec<_>>>()
        }) else {
            continue;
        };
        if accounts.first().is_none_or(|k| k != market) {
            continue;
        }
        let Some(data) = ix["data"]
            .as_str()
            .and_then(|d| bs58::decode(d).into_vec().ok())
        else {
            continue;
        };
        let native_reserve = data.starts_with(&crate::protocol::RESERVE_BOOKING_DISCRIMINATOR);
        let native_claim = data.starts_with(&crate::protocol::CLAIM_BOOKING_DISCRIMINATOR);
        let charge = match (data.first(), data.len(), accounts.len()) {
            (_, 155, 10) if native_reserve && data[154] == 0 => None,
            (_, 116, 10) if native_reserve && data[115] == 0 => None,
            (_, 21, 11 | 13) if native_claim && data[20] == 0 => Some(
                transfer_charge(tx, index, &keys, &accounts[6], None, mint).ok_or(
                    Error::unavailable("Settlement transfer history unavailable"),
                )?,
            ),
            (Some(6), 108 | 116, 8 | 9) if data.len() == 108 || u64_at(&data, 108) == 0 => None,
            (Some(7), 13, 11) => Some(
                transfer_charge(tx, index, &keys, &accounts[6], Some(&accounts[7]), mint).ok_or(
                    Error::unavailable("Settlement transfer history unavailable"),
                )?,
            ),
            _ => continue,
        };
        events.push(Event {
            ix: index,
            accounts,
            data,
            charge,
        });
    }
    Ok(events)
}
fn transfer_charge(
    tx: &Value,
    index: usize,
    keys: &[String],
    source: &str,
    payout: Option<&str>,
    mint: &str,
) -> Option<u64> {
    let mut charge = 0u64;
    for ix in tx["meta"]["innerInstructions"]
        .as_array()?
        .iter()
        .filter(|entry| entry["index"].as_u64() == Some(index as u64))
        .flat_map(|entry| entry["instructions"].as_array().into_iter().flatten())
    {
        if keys.get(ix["programIdIndex"].as_u64()? as usize)? != crate::protocol::TOKEN {
            continue;
        }
        let data = bs58::decode(ix["data"].as_str()?).into_vec().ok()?;
        let accounts = ix["accounts"].as_array()?;
        let account = |at: usize| {
            keys.get(accounts.get(at)?.as_u64()? as usize)
                .map(String::as_str)
        };
        let target = match (data.first(), data.len()) {
            (Some(3), 9) => account(1),
            (Some(12), 10) if account(1) == Some(mint) && data[9] == 6 => account(2),
            _ => continue,
        };
        if account(0) == Some(source)
            && target.is_some()
            && payout.is_none_or(|payout| target == Some(payout))
        {
            charge = charge.checked_add(u64_at(&data, 1))?;
        }
    }
    Some(charge)
}
pub async fn order(app: &App, slot: u64, signature: &str, blocks: &mut Blocks) -> Result<u32> {
    if !blocks.contains_key(&slot) {
        let block=app.rpc.call("getBlock",json!([slot,{"commitment":"finalized","transactionDetails":"signatures","rewards":false,"maxSupportedTransactionVersion":0}])).await?;
        let signatures = block["signatures"]
            .as_array()
            .filter(|s| s.len() <= 20_000)
            .ok_or(Error::unavailable("Block transaction order unavailable"))?;
        let mut order = HashMap::with_capacity(signatures.len());
        for (index, value) in signatures.iter().enumerate() {
            let signature = value
                .as_str()
                .filter(|s| bs58::decode(s).into_vec().is_ok_and(|v| v.len() == 64))
                .ok_or(Error::unavailable("Invalid block signature"))?;
            if order.insert(signature.to_owned(), index as u32).is_some() {
                return Err(Error::unavailable("Duplicate block signature"));
            }
        }
        if blocks.len() >= 8 {
            blocks.clear();
        }
        blocks.insert(slot, order);
    }
    let index = blocks[&slot].get(signature).copied();
    if index.is_none() {
        blocks.remove(&slot);
    }
    index.ok_or(Error::unavailable(
        "Transaction missing from finalized block",
    ))
}
pub fn store(
    tx: &rusqlite::Transaction<'_>,
    signature: &str,
    slot: u64,
    order: u32,
    time: i64,
    events: &[Event],
) -> Result<()> {
    for event in events {
        let id = format!("{signature}:{}", event.ix);
        if let Some(charge) = event.charge {
            let tokens = if event
                .data
                .starts_with(&crate::protocol::CLAIM_BOOKING_DISCRIMINATOR)
            {
                [8, 12, 16]
            } else {
                [1, 5, 9]
            }
            .into_iter()
            .map(|at| u32_at(&event.data, at) as u64)
            .sum::<u64>();
            tx.execute("INSERT OR IGNORE INTO one_shot_claims(id,booking,lane,model,provider,slot,tx_order,ix,time,tokens,charge) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![id,event.accounts[4],event.accounts[3],event.accounts[1],event.accounts[5],slot,order,event.ix,time,tokens.to_string(),charge.to_string()])?;
        } else {
            tx.execute("UPDATE one_shot_claims SET assigned=0 WHERE assigned=-1 AND booking=?1 AND lane=?2",params![event.accounts[6],event.accounts[4]])?;
            tx.execute(
                "INSERT OR IGNORE INTO one_shot_bookings VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    id,
                    event.accounts[6],
                    event.accounts[4],
                    slot,
                    order,
                    event.ix,
                    event.accounts[5],
                    event.accounts[1],
                    event.accounts.get(8).map(String::as_str).unwrap_or("")
                ],
            )?;
        }
    }
    Ok(())
}
pub async fn resolve(app: &App) -> Result<()> {
    app.db.call(|conn| {
        // wait until discovery and processing have covered the interval; otherwise an older
        // reservation could be mistaken for a newer reservation still in the history queue.
        if !usage::ready(conn)? {return Ok(());}
        let tx=conn.transaction()?;
        let mut query=tx.prepare_cached("SELECT id,booking,lane,model,provider,slot,tx_order,ix,tokens,charge FROM one_shot_claims WHERE assigned=0 ORDER BY slot,tx_order,ix LIMIT 256")?;
        let rows=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,u64>(5)?,r.get::<_,u32>(6)?,r.get::<_,u32>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        drop(query);
        for (id,booking,lane,model,provider,slot,order,ix,tokens,charge) in rows {
            let binding=tx.query_row("SELECT owner,model,list FROM one_shot_bookings WHERE booking=?1 AND lane=?2 AND (slot<?3 OR slot=?3 AND (tx_order<?4 OR tx_order=?4 AND ix<?5)) ORDER BY slot DESC,tx_order DESC,ix DESC LIMIT 1",params![booking,lane,slot,order,ix],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
            let Some((owner,_,list))=binding.filter(|(_,m,_)|m==&model) else {tx.execute("UPDATE one_shot_claims SET assigned=-1 WHERE id=?1",[id])?;continue};
            let tokens=tokens.parse::<u64>().map_err(|_|Error::internal())?;
            usage::add(&tx,&format!("provider:{provider}"),&model,&provider,tokens,&charge)?;
            if !list.is_empty() {usage::add(&tx,&format!("list:{list}"),&model,&provider,tokens,&charge)?;}
            tx.execute("UPDATE one_shot_claims SET assigned=1,owner=?1,list=?2 WHERE id=?3",params![owner,list,id])?;
        }
        tx.commit()?;Ok(())
    }).await
}
