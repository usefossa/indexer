mod grpc;
mod ws;
use crate::{
    App,
    error::{Error, Result},
    index::Record,
};
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;

pub enum Event {
    Connected,
    Activity(u64),
    Accounts(Vec<(String, Record)>),
}
type Snapshot = (u64, Vec<(String, Record)>);
async fn snapshot(app: App, boundary: u64, clock: u64) -> Result<(u64, Vec<(String, Record)>)> {
    let genesis = app.rpc.call("getGenesisHash", json!([])).await?;
    if genesis != app.config.genesis {
        return Err(Error::unavailable(
            "RPC network does not match configured genesis hash",
        ));
    }
    let minimum = {
        let state = app.index.state.read().await;
        state.slot.max(boundary).max(if clock > state.floor {
            state.floor.saturating_add(1)
        } else {
            state.floor
        })
    };
    let result=app.rpc.call("getProgramAccounts",json!([app.config.program.to_string(),{"encoding":"base64","commitment":app.config.commitment,"withContext":true,"minContextSlot":minimum}])).await?;
    let slot = result["context"]["slot"]
        .as_u64()
        .filter(|s| *s >= minimum)
        .ok_or(Error::unavailable(
            "RPC snapshot lacks a valid context slot",
        ))?;
    let values = result["value"]
        .as_array()
        .filter(|v| v.len() <= app.config.max_accounts)
        .ok_or(Error::unavailable(
            "RPC snapshot exceeds configured account limit",
        ))?;
    let mut records = Vec::with_capacity(values.len());
    for value in values {
        let address = value["pubkey"]
            .as_str()
            .ok_or(Error::unavailable("Invalid snapshot address"))?;
        crate::protocol::pubkey(address)?;
        records.push((
            address.into(),
            Record::from_rpc(&value["account"], slot, 0)?,
        ));
    }
    Ok((slot, records))
}
async fn session(app: &App) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(2048);
    let config = app.config.clone();
    let mut source = tokio::spawn(async move {
        if config.grpc_url.is_some() {
            grpc::connect(config, tx).await
        } else {
            ws::connect(config, tx).await
        }
    });
    let mut reconciliation: Option<tokio::task::JoinHandle<Result<Snapshot>>> = None;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut since = std::time::Instant::now();
    let mut connected = false;
    let mut boundary = 0;
    let mut clock = 0;
    let mut attempted = false;
    let result = loop {
        tokio::select! {
            result=&mut source=>break result.map_err(|_|Error::unavailable("Subscription task stopped"))?,
            event=rx.recv()=>match event {
                Some(Event::Connected)=>connected=true,
                Some(Event::Activity(slot))=>{if boundary==0 {boundary=slot;}clock=clock.max(slot);app.index.activity().await;},
                Some(Event::Accounts(mut records))=>{
                    while records.len()<256 {match rx.try_recv(){Ok(Event::Accounts(more))=>records.extend(more),Ok(Event::Activity(slot))=>{if boundary==0 {boundary=slot;}clock=clock.max(slot);app.index.activity().await;},Ok(Event::Connected)=>connected=true,Err(_)=>break}}
                    if let Err(error)=app.index.apply(&app.db,&app.config,records,None).await {break Err(error);}
                }, None=>break Err(Error::unavailable("Subscription closed")),
            },
            result=async{reconciliation.as_mut().expect("guarded task").await},if reconciliation.is_some()=>{
                reconciliation=None;since=std::time::Instant::now();
                match result.map_err(|_|Error::unavailable("Snapshot task stopped")).and_then(|r|r) {
                    Ok((slot,records))=>match app.index.apply(&app.db,&app.config,records,Some(slot)).await {Ok(())=>app.index.connected(true).await,Err(error)=>{app.index.connected(false).await;tracing::warn!(%error,"snapshot rejected");}},
                    Err(error)=>{app.index.connected(false).await;tracing::warn!(%error,"reconciliation unavailable");}
                }
            },
            _=ticker.tick()=>{
                let stale={let s=app.index.state.read().await;s.activity.is_some_and(|at|at.elapsed()>app.config.stale)};
                if stale {break Err(Error::unavailable("Subscription stalled"));}
                app.index.notify();
            }
        }
        let retry = if app.index.ready(&app.config).await {
            app.config.reconcile
        } else {
            Duration::from_secs(1)
        };
        if connected
            && boundary > 0
            && reconciliation.is_none()
            && (!attempted || since.elapsed() >= retry)
        {
            attempted = true;
            reconciliation = Some(tokio::spawn(snapshot(app.clone(), boundary, clock)));
        }
    };
    source.abort();
    if let Some(task) = reconciliation {
        task.abort();
    }
    app.index.connected(false).await;
    result
}
pub async fn run(app: App) {
    let mut delay = 1;
    loop {
        let start = std::time::Instant::now();
        if let Err(error) = session(&app).await {
            tracing::warn!(%error,retry_seconds=delay,"chain disconnected");
        }
        if start.elapsed() > Duration::from_secs(60) {
            delay = 1;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}
