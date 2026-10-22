use super::wallet;
use crate::{
    App, auth,
    error::{Error, Result},
    index::Record,
    protocol,
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, Utf8Bytes, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub async fn upgrade(
    State(app): State<App>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    auth::session(&app, &headers).await?;
    let permit = app.sockets.clone().try_acquire_owned().map_err(|_| {
        Error(
            StatusCode::TOO_MANY_REQUESTS,
            "Live connection limit reached",
        )
    })?;
    Ok(ws
        .max_message_size(1024)
        .max_frame_size(1024)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            let _ = stream(app, headers, socket).await;
        }))
}
async fn send(socket: &mut WebSocket, text: Utf8Bytes) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(text)))
        .await
        .map_err(|_| Error::unavailable("Slow live connection"))?
        .map_err(|_| Error::unavailable("Live connection closed"))
}
async fn watches(
    app: &App,
    socket: &mut WebSocket,
    watches: &mut HashMap<String, Option<(u64, u64)>>,
) -> Result<()> {
    let mut records = Vec::new();
    {
        let state = app.index.state.read().await;
        for (address, previous) in watches.iter() {
            if let Some(record) = state.accounts.get(address)
                && Some((record.slot, record.version)) != *previous
            {
                records.push((address.clone(), record.clone()));
            }
        }
    }
    send_records(socket, watches, records).await
}
async fn send_records(
    socket: &mut WebSocket,
    watches: &mut HashMap<String, Option<(u64, u64)>>,
    records: Vec<(String, Record)>,
) -> Result<()> {
    for (address, record) in records {
        if !watches.contains_key(&address) {
            continue;
        }
        if watches
            .get(&address)
            .is_some_and(|old| old.is_some_and(|old| (record.slot, record.version) < old))
        {
            continue;
        }
        send(
            socket,
            json!({"type":"booking","address":address,"data":record.envelope()})
                .to_string()
                .into(),
        )
        .await?;
        watches.insert(address, Some((record.slot, record.version)));
    }
    Ok(())
}
async fn stream(app: App, headers: HeaderMap, mut socket: WebSocket) -> Result<()> {
    let mut changes = app.index.changes.subscribe();
    let mut quotes = app.chain.subscribe();
    let mut shutdown = app.shutdown.subscribe();
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watched = HashMap::new();
    let mut input_count = 0usize;
    let mut input_window = Instant::now();
    let mut last_catalog = app.index.catalog(&app.config).await;
    send(&mut socket, last_catalog.message.clone()).await?;
    let mut snapshots = tokio::task::JoinSet::new();
    let mut fallbacks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
        _ = shutdown.changed() => return Ok(()),
            incoming=socket.recv()=>match incoming {
                Some(Ok(Message::Text(text)))=>{
                    if input_window.elapsed()>=Duration::from_secs(60){input_window=Instant::now();input_count=0;}input_count+=1;if input_count>120{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"Too many watch requests"));}
                    let value:Value=serde_json::from_str(&text)?;
                    if value.as_object().is_none_or(|v|v.len()!=1) {return Err(Error::bad("Use watch or unwatch"));}
                    if let Some(address)=value["watch"].as_str(){protocol::pubkey(address)?;if !watched.contains_key(address)&&watched.len()>=64{return Err(Error::bad("Watch limit reached"));}watched.entry(address.into()).or_insert(None);watches(&app,&mut socket,&mut watched).await?;start_fallback(&app,&watched,&mut fallbacks).await;}
                    else if let Some(address)=value["unwatch"].as_str(){watched.remove(address);}else{return Err(Error::bad("Invalid watch request"));}
                },
                Some(Ok(Message::Ping(bytes)))=>{tokio::time::timeout(Duration::from_secs(5),socket.send(Message::Pong(bytes))).await.map_err(|_|Error::unavailable("Live connection stalled"))?.map_err(|_|Error::unavailable("Live connection closed"))?;},
                None|Some(Err(_))|Some(Ok(Message::Close(_)))=>return Ok(()),_=>{}
            },
            change=changes.changed()=>{if change.is_err(){return Ok(());}let catalog=app.index.catalog(&app.config).await;if !std::sync::Arc::ptr_eq(&catalog,&last_catalog){send(&mut socket,catalog.message.clone()).await?;last_catalog=catalog;}watches(&app,&mut socket,&mut watched).await?;},
            change=quotes.changed()=>{if change.is_err(){return Ok(());}let value=quotes.borrow_and_update().clone();send(&mut socket,json!({"type":"chain","data":value}).to_string().into()).await?;},
            _=ticker.tick()=>{
                let session=auth::session(&app,&headers).await?;
                let catalog=app.index.catalog(&app.config).await;
                send(&mut socket,catalog.message.clone()).await?;last_catalog=catalog;
                if !last_catalog.ready {send(&mut socket,json!({"type":"chain","data":null}).to_string().into()).await?;}
                if snapshots.is_empty(){let app=app.clone();snapshots.spawn(async move {wallet::snapshot(&app,&session).await});}
                watches(&app,&mut socket,&mut watched).await?;start_fallback(&app,&watched,&mut fallbacks).await;
            },
            Some(result)=fallbacks.join_next(),if !fallbacks.is_empty()=>{if let Ok(Ok(records))=result{send_records(&mut socket,&mut watched,records).await?;}},
            Some(result)=snapshots.join_next(),if !snapshots.is_empty()=>{
                let value=result.map_err(|_|Error::unavailable("Wallet synchronization stopped"))??;
                send(&mut socket,json!({"type":"wallet","data":value}).to_string().into()).await?;
            }
        }
    }
}

async fn start_fallback(
    app: &App,
    watches: &HashMap<String, Option<(u64, u64)>>,
    tasks: &mut tokio::task::JoinSet<Result<Vec<(String, Record)>>>,
) {
    if !tasks.is_empty() {
        return;
    }
    let addresses = {
        let state = app.index.state.read().await;
        watches
            .keys()
            .filter(|address| !state.accounts.contains_key(*address))
            .cloned()
            .collect::<Vec<_>>()
    };
    if addresses.is_empty() {
        return;
    }
    let app = app.clone();
    tasks.spawn(async move {
        let minimum=app.index.state.read().await.floor;
        let result=app.rpc.call("getMultipleAccounts",json!([addresses,{"encoding":"base64","commitment":app.config.commitment,"minContextSlot":minimum}])).await?;
        let slot=result["context"]["slot"].as_u64().filter(|s|*s>=minimum).ok_or(Error::unavailable("Watch snapshot regressed"))?;
        let values=result["value"].as_array().filter(|v|v.len()==addresses.len()).ok_or(Error::unavailable("Watch snapshot incomplete"))?;
        addresses.into_iter().zip(values).map(|(address,value)|{
            let mut record=Record::from_rpc(value,slot,0)?;
            if !record.data.is_empty()&&(record.owner!=app.config.program.to_string()||protocol::validate(&address,&record.data,&app.config)!=Some(protocol::Kind::Booking)){record=Record::closed(slot,0);}
            Ok((address,record))
        }).collect()
    });
}
