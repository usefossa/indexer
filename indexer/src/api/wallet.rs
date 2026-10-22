use crate::{
    App,
    auth::{self, Session},
    error::{Error, Result},
    protocol::{self, Kind, address, i64_at, u32_at, u64_at},
    receipts,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub async fn snapshot(app: &App, session: &Session) -> Result<Value> {
    let owner = protocol::pubkey(&session.wallet)?;
    let lane = protocol::pubkey(&session.lane)?;
    let source = protocol::associated(&owner, &app.config.mint);
    let vault = protocol::vault(&lane, &app.config);
    let minimum = app.index.state.read().await.floor;
    let result=app.rpc.call("getMultipleAccounts",json!([[session.wallet,source.to_string(),session.lane],{"encoding":"base64","commitment":app.config.commitment,"minContextSlot":minimum}])).await?;
    let slot = result["context"]["slot"]
        .as_u64()
        .filter(|s| *s >= minimum)
        .ok_or(Error::unavailable("Wallet snapshot regressed"))?;
    let values = result["value"]
        .as_array()
        .filter(|v| v.len() == 3)
        .ok_or(Error::unavailable("Incomplete wallet snapshot"))?;
    let mut free = 0;
    let mut generation = 0;
    let initialized = if values[2].is_null() {
        false
    } else {
        let data =
            protocol::bytes(&values[2]).ok_or(Error::unavailable("Invalid user account data"))?;
        if values[2]["owner"] != app.config.program.to_string()
            || protocol::validate(&session.lane, &data, &app.config) != Some(Kind::User)
            || address(&data, 8) != session.wallet
        {
            return Err(Error(
                StatusCode::FORBIDDEN,
                "User account belongs to another wallet or protocol",
            ));
        }
        free = u64_at(&data, 72);
        let mut bytes = [0; 8];
        bytes[..7].copy_from_slice(&data[113..120]);
        generation = u64::from_le_bytes(bytes);
        true
    };
    let usdc = if values[1].is_null() {
        0
    } else {
        let data =
            protocol::bytes(&values[1]).ok_or(Error::unavailable("Invalid token account"))?;
        if values[1]["owner"] != protocol::TOKEN
            || data.len() != 165
            || protocol::key(&data, 0) != Some(app.config.mint)
            || protocol::key(&data, 32) != Some(owner)
            || data[108] != 1
        {
            return Err(Error::unavailable("Invalid or frozen USDC source account"));
        }
        u64_at(&data, 64)
    };
    let lamports = if values[0].is_null() {
        0
    } else {
        values[0]["lamports"]
            .as_u64()
            .ok_or(Error::unavailable("Invalid wallet balance"))?
    };
    let bookings = booking_pool(app, session).await;
    Ok(
        json!({"wallet":session.wallet,"lane":session.lane,"vault":vault.to_string(),"source":source.to_string(),"ready":initialized,"lamports":lamports.to_string(),"usdcAtomic":usdc.to_string(),"freeAtomic":free.to_string(),"generation":generation.to_string(),"bookings":bookings,"slot":slot,"slots":[slot,slot,slot]}),
    )
}
pub async fn booking_pool(app: &App, session: &Session) -> Vec<Value> {
    let state = app.index.state.read().await;
    let mut bookings = Vec::new();
    for (id, r) in &state.accounts {
        let d = &r.data;
        if protocol::kind(d) != Some(Kind::Booking)
            || address(d, 8) != session.wallet
            || address(d, 192) != session.lane
            || d[359] == 0xa5
            || (d[148] != 5 && u64_at(d, 152) == 0)
        {
            continue;
        }
        bookings.push(json!({"address":id,"slot":r.slot,"status":d[148],"generation":u64_at(d,152).to_string(),"rentPayer":address(d,256)}));
    }
    bookings.sort_by(|a, b| a["address"].as_str().cmp(&b["address"].as_str()));
    bookings
}
pub async fn user_orders(app: &App, wallet: &str, lane: Option<&str>) -> Vec<Value> {
    let s = app.index.state.read().await;
    let mut orders = Vec::new();
    for (id, r) in &s.accounts {
        let d = &r.data;
        if protocol::kind(d) != Some(Kind::Booking)
            || address(d, 8) != wallet
            || lane.is_some_and(|l| l != address(d, 192))
            || d[359] == 0xa5
        {
            continue;
        }
        orders.push(json!({"address":id,"slot":r.slot,"provider":address(d,40),"model":address(d,224),"lane":address(d,192),"generation":u64_at(d,152).to_string(),"offerSlot":u32_at(d,352),"status":d[148],"reserve":u64_at(d,104).to_string(),"charge":u64_at(d,160).to_string(),"uncachedTokens":u32_at(d,168),"cachedTokens":u32_at(d,172),"outputTokens":u32_at(d,176),"claimEnd":i64_at(d,132),"disputeEnd":i64_at(d,140)}));
    }
    orders.sort_by_key(|v| std::cmp::Reverse(v["slot"].as_u64().unwrap_or(0)));
    orders
}
pub async fn wallet(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let session = auth::session(&app, &headers).await?;
    Ok(Json(snapshot(&app, &session).await?))
}
pub async fn funds(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let session = auth::session(&app, &headers).await?;
    if !app.index.ready(&app.config).await {
        return Err(Error::unavailable("Indexer synchronizing"));
    }
    let snapshot = snapshot(&app, &session).await?;
    let now = crate::now() as i64;
    let bookings: Vec<_> = user_orders(&app, &session.wallet, Some(&session.lane))
        .await
        .into_iter()
        .filter(|v| {
            v["status"] == 0
                && v["claimEnd"]
                    .as_i64()
                    .is_some_and(|end| end.saturating_add(15) < now)
        })
        .collect();
    let expired = bookings
        .iter()
        .try_fold(0u64, |sum, v| {
            sum.checked_add(v["reserve"].as_str()?.parse::<u64>().ok()?)
        })
        .ok_or(Error::unavailable("Balance overflow"))?;
    Ok(Json(
        json!({"free":snapshot["freeAtomic"],"expired":expired.to_string(),"expiredBookings":bookings.len(),"bookings":bookings}),
    ))
}
pub async fn orders(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let session = auth::session(&app, &headers).await?;
    Ok(Json(
        json!({"ready":app.index.ready(&app.config).await,"orders":user_orders(&app,&session.wallet,Some(&session.lane)).await}),
    ))
}
pub async fn profile(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let session = auth::session(&app, &headers).await?;
    Ok(Json(
        json!({"ready":app.index.ready(&app.config).await,"wallet":snapshot(&app,&session).await?,"orders":user_orders(&app,&session.wallet,Some(&session.lane)).await,"receipts":receipts::history(&app,&session.wallet,false).await?}),
    ))
}
pub async fn provider(State(app): State<App>, Path(wallet): Path<String>) -> Result<Json<Value>> {
    protocol::pubkey(&wallet)?;
    let catalog = app.index.catalog(&app.config).await;
    let provider = catalog.value["providers"]
        .as_array()
        .and_then(|p| p.iter().find(|v| v["wallet"] == wallet))
        .cloned()
        .ok_or(Error(StatusCode::NOT_FOUND, "Provider unavailable"))?;
    Ok(Json(
        json!({"ready":catalog.ready,"provider":provider,"receipts":receipts::history(&app,&wallet,true).await?}),
    ))
}
pub async fn quotes(app: App) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        let value = quote(&app).await.ok().map(Arc::new);
        app.chain.send_replace(value);
    }
}
async fn quote(app: &App) -> Result<Value> {
    if !app.index.ready(&app.config).await {
        return Err(Error::unavailable("Indexer synchronizing"));
    }
    let (block, rent) = tokio::try_join!(
        app.rpc.call(
            "getLatestBlockhash",
            json!([{"commitment":app.config.commitment}])
        ),
        app.rpc.call(
            "getMinimumBalanceForRentExemption",
            json!([protocol::BOOKING_LEN,{"commitment":app.config.commitment}])
        )
    )?;
    let blockhash = block["value"]["blockhash"]
        .as_str()
        .ok_or(Error::unavailable("Blockhash unavailable"))?;
    let hash = protocol::pubkey(blockhash)?;
    let mut message = vec![1, 0, 0, 1];
    message.extend_from_slice(app.config.market.as_ref());
    message.extend_from_slice(hash.as_ref());
    message.push(0);
    let fee = app
        .rpc
        .call(
            "getFeeForMessage",
            json!([STANDARD.encode(message),{"commitment":app.config.commitment}]),
        )
        .await?;
    Ok(
        json!({"blockhash":blockhash,"lastValidBlockHeight":block["value"]["lastValidBlockHeight"].as_u64().ok_or(Error::unavailable("Block height unavailable"))?,"bookingRent":rent.as_u64().ok_or(Error::unavailable("Booking rent unavailable"))?.to_string(),"signatureFee":fee["value"].as_u64().ok_or(Error::unavailable("Signature fee unavailable"))?.to_string(),"maxAgeMs":15_000}),
    )
}
