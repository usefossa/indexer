use super::Event;
use crate::{
    config::Config,
    error::{Error, Result},
    fees::Fees,
    index::Record,
};
use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request,
    transport::{ClientTlsConfig, Endpoint},
};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions, SubscribeRequestPing, SubscribeUpdateTransaction,
    geyser_client::GeyserClient, subscribe_update::UpdateOneof,
};

const COMPUTE: &str = "ComputeBudget111111111111111111111111111111";
static COMPUTE_KEY: LazyLock<Vec<u8>> =
    LazyLock::new(|| bs58::decode(COMPUTE).into_vec().expect("compute program"));

#[cfg(test)]
#[path = "grpc_tests.rs"]
mod tests;

pub async fn connect(config: Config, tx: mpsc::Sender<Event>, fees: Fees) -> Result<()> {
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
    let mut client = GeyserClient::new(channel)
        .max_decoding_message_size(crate::protocol::MAX_ACCOUNT_BYTES + 65536);
    let mut fee_stream = true;
    let (mut requests, mut stream) = match subscribe(&mut client, &config, true).await {
        Ok(subscription) => subscription,
        Err(status) if unsupported_fees(&status) => {
            fee_stream = false;
            subscribe(&mut client, &config, false)
                .await
                .map_err(|_| Error::unavailable("gRPC subscription rejected"))?
        }
        Err(_) => return Err(Error::unavailable("gRPC subscription rejected")),
    };
    tx.send(Event::Connected)
        .await
        .map_err(|_| Error::unavailable("Indexer stopped"))?;
    let mut ping = tokio::time::interval(Duration::from_secs(10));
    let mut last = Instant::now();
    let mut slot = 0;
    let mut received_state = false;
    loop {
        tokio::select! {
            update=stream.message()=>{let update=match update {
                Ok(Some(update))=>update,
                Err(status) if fee_stream && !received_state && unsupported_fees(&status)=>{
                    (requests,stream)=subscribe(&mut client,&config,false).await.map_err(|_|Error::unavailable("gRPC subscription rejected"))?;
                    fee_stream=false;
                    continue;
                },
                Err(_)=>return Err(Error::unavailable("gRPC stream failed")),
                Ok(None)=>return Err(Error::unavailable("gRPC stream closed")),
            };let created_at=update.created_at.filter(|value|(0..1_000_000_000).contains(&value.nanos)).and_then(|value|u64::try_from(value.seconds).ok());match update.update_oneof {
                Some(UpdateOneof::Account(value))=>{let account=value.account.ok_or(Error::unavailable("Missing gRPC account"))?;if account.pubkey.len()!=32||account.owner.len()!=32||account.data.len()>crate::protocol::MAX_ACCOUNT_BYTES {return Err(Error::unavailable("Invalid gRPC account"));}received_state=true;let address=bs58::encode(account.pubkey).into_string();let record=if account.lamports==0||account.executable {Record::closed(value.slot,account.write_version)}else{Record{slot:value.slot,version:account.write_version,owner:bs58::encode(account.owner).into_string(),lamports:account.lamports,data:account.data.into()}};tx.send(Event::Accounts(vec![(address,record)])).await.map_err(|_|Error::unavailable("Indexer stopped"))?;},
                Some(UpdateOneof::Slot(value))=>{if value.slot>slot {received_state=true;slot=value.slot;last=Instant::now();tx.send(Event::Activity(slot)).await.map_err(|_|Error::unavailable("Indexer stopped"))?;}},
                Some(UpdateOneof::Transaction(value))=>{if let Some((slot,signature,price))=priority(value)&&let Some(created_at)=created_at {fees.observe(slot,signature,created_at,price).await;}},
                Some(UpdateOneof::Ping(_))=>requests.send(SubscribeRequest{ping:Some(SubscribeRequestPing{id:1}),..Default::default()}).await.map_err(|_|Error::unavailable("gRPC heartbeat failed"))?,_=>{}
            }},
            _=ping.tick()=>{if last.elapsed()>config.stale {return Err(Error::unavailable("gRPC slots stalled"));}requests.send(SubscribeRequest{ping:Some(SubscribeRequestPing{id:1}),..Default::default()}).await.map_err(|_|Error::unavailable("gRPC heartbeat failed"))?;}
        }
    }
}

type Subscription = (
    mpsc::Sender<SubscribeRequest>,
    tonic::Streaming<yellowstone_grpc_proto::prelude::SubscribeUpdate>,
);

fn unsupported_fees(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::InvalidArgument
            | tonic::Code::Unimplemented
            | tonic::Code::PermissionDenied
            | tonic::Code::ResourceExhausted
    )
}

async fn subscribe(
    client: &mut GeyserClient<tonic::transport::Channel>,
    config: &Config,
    transaction_fees: bool,
) -> std::result::Result<Subscription, tonic::Status> {
    let (requests, receiver) = mpsc::channel(8);
    requests
        .send(SubscribeRequest {
            accounts: std::collections::HashMap::from([
                (
                    "protocol".into(),
                    SubscribeRequestFilterAccounts {
                        owner: vec![config.program.to_string()],
                        ..Default::default()
                    },
                ),
                (
                    "provider_list".into(),
                    SubscribeRequestFilterAccounts {
                        account: vec![config.provider_list.to_string()],
                        ..Default::default()
                    },
                ),
            ]),
            slots: std::collections::HashMap::from([(
                "clock".into(),
                SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..Default::default()
                },
            )]),
            transactions: if transaction_fees {
                std::collections::HashMap::from([(
                    "priority_fees".into(),
                    SubscribeRequestFilterTransactions {
                        vote: Some(false),
                        failed: Some(false),
                        account_include: vec![COMPUTE.into()],
                        account_required: vec![config.program.to_string()],
                        ..Default::default()
                    },
                )])
            } else {
                Default::default()
            },
            commitment: Some(if config.commitment == "finalized" {
                CommitmentLevel::Finalized as i32
            } else {
                CommitmentLevel::Confirmed as i32
            }),
            ..Default::default()
        })
        .await
        .map_err(|_| tonic::Status::cancelled("gRPC subscription failed"))?;
    let mut request = Request::new(ReceiverStream::new(receiver));
    if let Some(token) = &config.grpc_token {
        request.metadata_mut().insert(
            "x-token",
            token
                .parse()
                .map_err(|_| tonic::Status::invalid_argument("Invalid gRPC token metadata"))?,
        );
    }
    let stream = tokio::time::timeout(Duration::from_secs(10), client.subscribe(request))
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("gRPC subscribe timed out"))??
        .into_inner();
    Ok((requests, stream))
}

fn priority(update: SubscribeUpdateTransaction) -> Option<(u64, [u8; 64], u64)> {
    let info = update.transaction.filter(|info| !info.is_vote)?;
    let signature = info.signature.as_slice().try_into().ok()?;
    let meta = info.meta.filter(|meta| meta.err.is_none())?;
    let message = info
        .transaction?
        .message
        .filter(|message| message.config.is_none())?;
    if message
        .account_keys
        .len()
        .saturating_add(meta.loaded_writable_addresses.len())
        .saturating_add(meta.loaded_readonly_addresses.len())
        > 256
        || message.instructions.len() > 256
    {
        return None;
    }
    let mut keys = message
        .account_keys
        .iter()
        .chain(meta.loaded_writable_addresses.iter())
        .chain(meta.loaded_readonly_addresses.iter());
    if keys.clone().any(|key| key.len() != 32) {
        return None;
    }
    let index = keys.position(|key| key == &*COMPUTE_KEY)?;
    let mut price = None;
    for instruction in message.instructions {
        if instruction.program_id_index as usize != index || instruction.data.first() != Some(&3) {
            continue;
        }
        if !instruction.accounts.is_empty() || price.is_some() {
            return None;
        }
        price = Some(u64::from_le_bytes(
            instruction.data.get(1..)?.try_into().ok()?,
        ));
    }
    Some((update.slot, signature, price.unwrap_or(0)))
}
