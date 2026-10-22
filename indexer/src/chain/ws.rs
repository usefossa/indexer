use super::Event;
use crate::{
    config::Config,
    error::{Error, Result},
    index::Record,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

pub async fn connect(config: Config, tx: mpsc::Sender<Event>) -> Result<()> {
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(2 * 1024 * 1024))
        .max_frame_size(Some(2 * 1024 * 1024));
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_async_with_config(
            config
                .ws_url
                .as_deref()
                .ok_or(Error::bad("Missing websocket endpoint"))?,
            Some(ws_config),
            false,
        ),
    )
    .await
    .map_err(|_| Error::unavailable("Websocket connection timed out"))?
    .map_err(|_| Error::unavailable("Websocket connection failed"))?;
    for (id, method, params) in [
        (
            1,
            "programSubscribe",
            json!([config.program.to_string(),{"encoding":"base64","commitment":config.commitment}]),
        ),
        (2, "slotSubscribe", json!([])),
    ] {
        socket
            .send(Message::Text(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|_| Error::unavailable("Websocket subscribe failed"))?;
    }
    let mut acknowledgements = 0u8;
    let mut version = 0u64;
    let mut last_slot = 0u64;
    let opened = Instant::now();
    let mut last = Instant::now();
    let mut ping = tokio::time::interval(Duration::from_secs(5));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            message=socket.next()=>match message {
                Some(Ok(Message::Text(text)))=>{
                    let value:Value=serde_json::from_str(&text).map_err(|_|Error::unavailable("Invalid websocket JSON"))?;
                    if value.get("error").is_some() {return Err(Error::unavailable("Websocket subscription rejected"));}
                    if let Some(id)=value["id"].as_u64() {if (1..=2).contains(&id)&&value["result"].as_u64().is_some(){acknowledgements|=1<<(id-1);if acknowledgements==3 {tx.send(Event::Connected).await.map_err(|_|Error::unavailable("Indexer stopped"))?;acknowledgements=7;}}continue;}
                    match value["method"].as_str() {
                        Some("programNotification")=>{let result=&value["params"]["result"];let slot=result["context"]["slot"].as_u64().ok_or(Error::unavailable("Missing stream context"))?;let account=&result["value"];let address=account["pubkey"].as_str().ok_or(Error::unavailable("Missing stream address"))?;crate::protocol::pubkey(address)?;version=version.checked_add(1).ok_or(Error::unavailable("Stream sequence exhausted"))?;let record=Record::from_rpc(&account["account"],slot,version)?;tx.send(Event::Accounts(vec![(address.into(),record)])).await.map_err(|_|Error::unavailable("Indexer stopped"))?;},
                        Some("slotNotification")=>{let slot=value["params"]["result"]["slot"].as_u64().ok_or(Error::unavailable("Missing slot notification"))?;if slot>last_slot {last_slot=slot;last=Instant::now();tx.send(Event::Activity(slot)).await.map_err(|_|Error::unavailable("Indexer stopped"))?;}},_=>{}
                    }
                },
                Some(Ok(Message::Ping(bytes)))=>socket.send(Message::Pong(bytes)).await.map_err(|_|Error::unavailable("Websocket heartbeat failed"))?,
                Some(Ok(Message::Close(_)))|Some(Err(_))|None=>return Err(Error::unavailable("Websocket closed")),_=>{}
            },
            _=ping.tick()=>{if acknowledgements!=7&&opened.elapsed()>Duration::from_secs(10)||last.elapsed()>config.stale {return Err(Error::unavailable("Websocket stream stalled"));}tokio::time::timeout(Duration::from_secs(5),socket.send(Message::Ping(Vec::new().into()))).await.map_err(|_|Error::unavailable("Websocket heartbeat timed out"))?.map_err(|_|Error::unavailable("Websocket heartbeat failed"))?;}
        }
    }
}
