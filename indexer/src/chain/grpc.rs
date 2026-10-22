use super::Event;
use crate::{
    config::Config,
    error::{Error, Result},
    index::Record,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request,
    transport::{ClientTlsConfig, Endpoint},
};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots,
    SubscribeRequestPing, geyser_client::GeyserClient, subscribe_update::UpdateOneof,
};

#[cfg(test)]
#[path = "grpc_tests.rs"]
mod tests;

pub async fn connect(config: Config, tx: mpsc::Sender<Event>) -> Result<()> {
    let url = config
        .grpc_url
        .as_ref()
        .ok_or(Error::bad("Missing gRPC endpoint"))?;
    let mut endpoint = Endpoint::from_shared(url.clone())
        .map_err(|_| Error::bad("Invalid gRPC endpoint"))?
        .connect_timeout(Duration::from_secs(10))
        .http2_keep_alive_interval(Duration::from_secs(15))
        .keep_alive_timeout(Duration::from_secs(5))
        .keep_alive_while_idle(true);
    if url.starts_with("https:") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|_| Error::bad("Invalid gRPC TLS settings"))?;
    }
    let channel = endpoint
        .connect()
        .await
        .map_err(|_| Error::unavailable("gRPC connection failed"))?;
    let mut client = GeyserClient::new(channel).max_decoding_message_size(2 * 1024 * 1024);
    let (requests, receiver) = mpsc::channel(8);
    requests
        .send(SubscribeRequest {
            accounts: std::collections::HashMap::from([(
                "protocol".into(),
                SubscribeRequestFilterAccounts {
                    owner: vec![config.program.to_string()],
                    ..Default::default()
                },
            )]),
            slots: std::collections::HashMap::from([(
                "clock".into(),
                SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..Default::default()
                },
            )]),
            commitment: Some(if config.commitment == "finalized" {
                CommitmentLevel::Finalized as i32
            } else {
                CommitmentLevel::Confirmed as i32
            }),
            ..Default::default()
        })
        .await
        .map_err(|_| Error::unavailable("gRPC subscription failed"))?;
    let mut request = Request::new(ReceiverStream::new(receiver));
    if let Some(token) = &config.grpc_token {
        request.metadata_mut().insert(
            "x-token",
            token
                .parse()
                .map_err(|_| Error::bad("Invalid gRPC token metadata"))?,
        );
    }
    let mut stream = tokio::time::timeout(Duration::from_secs(10), client.subscribe(request))
        .await
        .map_err(|_| Error::unavailable("gRPC subscribe timed out"))?
        .map_err(|_| Error::unavailable("gRPC subscription rejected"))?
        .into_inner();
    tx.send(Event::Connected)
        .await
        .map_err(|_| Error::unavailable("Indexer stopped"))?;
    let mut ping = tokio::time::interval(Duration::from_secs(10));
    let mut last = Instant::now();
    let mut slot = 0;
    loop {
        tokio::select! {
            update=stream.message()=>{let update=update.map_err(|_|Error::unavailable("gRPC stream failed"))?.ok_or(Error::unavailable("gRPC stream closed"))?;match update.update_oneof {
                Some(UpdateOneof::Account(value))=>{let account=value.account.ok_or(Error::unavailable("Missing gRPC account"))?;if account.pubkey.len()!=32||account.owner.len()!=32||account.data.len()>491568 {return Err(Error::unavailable("Invalid gRPC account"));}let address=bs58::encode(account.pubkey).into_string();let record=if account.lamports==0||account.executable {Record::closed(value.slot,account.write_version)}else{Record{slot:value.slot,version:account.write_version,owner:bs58::encode(account.owner).into_string(),lamports:account.lamports,data:account.data.into()}};tx.send(Event::Accounts(vec![(address,record)])).await.map_err(|_|Error::unavailable("Indexer stopped"))?;},
                Some(UpdateOneof::Slot(value))=>{if value.slot>slot {slot=value.slot;last=Instant::now();tx.send(Event::Activity(slot)).await.map_err(|_|Error::unavailable("Indexer stopped"))?;}},
                Some(UpdateOneof::Ping(_))=>requests.send(SubscribeRequest{ping:Some(SubscribeRequestPing{id:1}),..Default::default()}).await.map_err(|_|Error::unavailable("gRPC heartbeat failed"))?,_=>{}
            }},
            _=ping.tick()=>{if last.elapsed()>config.stale {return Err(Error::unavailable("gRPC slots stalled"));}requests.send(SubscribeRequest{ping:Some(SubscribeRequestPing{id:1}),..Default::default()}).await.map_err(|_|Error::unavailable("gRPC heartbeat failed"))?;}
        }
    }
}
