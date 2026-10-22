use super::*;
use futures_util::stream;
use solana_pubkey::Pubkey;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tonic::{Response, Status, Streaming};
use yellowstone_grpc_proto::prelude::*;

#[derive(Clone)]
struct Mock {
    token: Option<String>,
    ping: Arc<AtomicBool>,
    program: String,
}
#[tonic::async_trait]
impl geyser_server::Geyser for Mock {
    type SubscribeStream = ReceiverStream<std::result::Result<SubscribeUpdate, Status>>;
    async fn subscribe(
        &self,
        request: Request<Streaming<SubscribeRequest>>,
    ) -> std::result::Result<Response<Self::SubscribeStream>, Status> {
        assert_eq!(
            request
                .metadata()
                .get("x-token")
                .map(|v| v.to_str().unwrap()),
            self.token.as_deref()
        );
        let mut input = request.into_inner();
        let first = input.message().await?.unwrap();
        assert_eq!(first.commitment, Some(CommitmentLevel::Confirmed as i32));
        assert_eq!(first.accounts["protocol"].owner, vec![self.program.clone()]);
        assert!(first.from_slot.is_none());
        assert!(first.slots["clock"].filter_by_commitment.unwrap());
        let (tx, rx) = mpsc::channel(8);
        let ping = self.ping.clone();
        tokio::spawn(async move {
            for update in [
                UpdateOneof::Slot(SubscribeUpdateSlot {
                    slot: 101,
                    status: 1,
                    ..Default::default()
                }),
                UpdateOneof::Account(SubscribeUpdateAccount {
                    slot: 101,
                    account: Some(SubscribeUpdateAccountInfo {
                        pubkey: vec![7; 32],
                        owner: vec![1; 32],
                        lamports: 55,
                        data: vec![2; 72],
                        write_version: 9,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                UpdateOneof::Ping(SubscribeUpdatePing {}),
            ] {
                if tx
                    .send(Ok(SubscribeUpdate {
                        update_oneof: Some(update),
                        ..Default::default()
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            while let Ok(Some(request)) = input.message().await {
                if request.ping.is_some() {
                    ping.store(true, Ordering::SeqCst);
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
    type SubscribeDeshredStream =
        ReceiverStream<std::result::Result<SubscribeUpdateDeshred, Status>>;
    async fn subscribe_deshred(
        &self,
        _: Request<Streaming<SubscribeDeshredRequest>>,
    ) -> std::result::Result<Response<Self::SubscribeDeshredStream>, Status> {
        Err(Status::unimplemented("unused"))
    }
    type SubscribeGossipStream = ReceiverStream<std::result::Result<SubscribeUpdateGossip, Status>>;
    async fn subscribe_gossip(
        &self,
        _: Request<SubscribeGossipRequest>,
    ) -> std::result::Result<Response<Self::SubscribeGossipStream>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn subscribe_replay_info(
        &self,
        _: Request<SubscribeReplayInfoRequest>,
    ) -> std::result::Result<Response<SubscribeReplayInfoResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn ping(
        &self,
        _: Request<PingRequest>,
    ) -> std::result::Result<Response<PongResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn get_latest_blockhash(
        &self,
        _: Request<GetLatestBlockhashRequest>,
    ) -> std::result::Result<Response<GetLatestBlockhashResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn get_block_height(
        &self,
        _: Request<GetBlockHeightRequest>,
    ) -> std::result::Result<Response<GetBlockHeightResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn get_slot(
        &self,
        _: Request<GetSlotRequest>,
    ) -> std::result::Result<Response<GetSlotResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn is_blockhash_valid(
        &self,
        _: Request<IsBlockhashValidRequest>,
    ) -> std::result::Result<Response<IsBlockhashValidResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn get_version(
        &self,
        _: Request<GetVersionRequest>,
    ) -> std::result::Result<Response<GetVersionResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
}
#[tokio::test]
async fn provider_neutral_wire_handles_optional_token_commitment_accounts_and_ping() {
    for token in [None, Some("local-test-token".to_owned())] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ping = Arc::new(AtomicBool::new(false));
        let program = Pubkey::new_from_array([1; 32]);
        let service = Mock {
            token: token.clone(),
            ping: ping.clone(),
            program: program.to_string(),
        };
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(geyser_server::GeyserServer::new(service))
                .serve_with_incoming(stream::unfold(listener, |listener| async {
                    let accepted = listener.accept().await.map(|(socket, _)| socket);
                    Some((accepted, listener))
                }))
                .await
                .unwrap();
        });
        let config = Config {
            rpc_url: "http://127.0.0.1:9".into(),
            ws_url: None,
            grpc_url: Some(format!("http://{address}")),
            grpc_token: token,
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "http://127.0.0.1:8790".into(),
            database: "unused".into(),
            program,
            market: Pubkey::new_from_array([2; 32]),
            mint: Pubkey::new_from_array([3; 32]),
            genesis: "test".into(),
            commitment: "confirmed".into(),
            reconcile: Duration::from_secs(1),
            stale: Duration::from_secs(3),
            max_accounts: 1000,
            max_rpc_bytes: 1_048_576,
            max_connections: 8,
            request_limit: 1000,
            auth_limit: 100,
            access_secs: 3600,
            refresh_secs: 86400,
        };
        let (tx, mut rx) = mpsc::channel(8);
        let client = tokio::spawn(connect(config, tx));
        tokio::time::timeout(Duration::from_secs(3), async {
            assert!(matches!(rx.recv().await, Some(Event::Connected)));
            assert!(matches!(rx.recv().await, Some(Event::Activity(101))));
            let Some(Event::Accounts(records)) = rx.recv().await else {
                panic!("missing account update")
            };
            assert_eq!(records[0].0, Pubkey::new_from_array([7; 32]).to_string());
            assert_eq!(records[0].1.slot, 101);
            assert_eq!(records[0].1.version, 9);
            assert_eq!(records[0].1.lamports, 55);
            assert_eq!(records[0].1.data.as_ref(), &[2; 72]);
            while !ping.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.abort();
        server.abort();
        let _ = client.await;
        let _ = server.await;
    }
}
