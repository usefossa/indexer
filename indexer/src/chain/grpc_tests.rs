use super::*;
use futures_util::stream;
use solana_pubkey::Pubkey;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tonic::{Response, Status, Streaming};
use yellowstone_grpc_proto::prelude::*;

#[derive(Clone)]
struct Mock {
    token: Option<String>,
    ping: Arc<AtomicBool>,
    program: String,
    reject_fees: bool,
    late_failure: bool,
    subscriptions: Arc<AtomicUsize>,
}
#[tonic::async_trait]
impl geyser_server::Geyser for Mock {
    type SubscribeStream = ReceiverStream<std::result::Result<SubscribeUpdate, Status>>;
    async fn subscribe(
        &self,
        request: Request<Streaming<SubscribeRequest>>,
    ) -> std::result::Result<Response<Self::SubscribeStream>, Status> {
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
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
        assert_eq!(
            first.accounts["provider_list"].account,
            vec![Pubkey::new_from_array([4; 32]).to_string()]
        );
        assert!(first.accounts["provider_list"].owner.is_empty());
        assert!(first.from_slot.is_none());
        assert!(first.slots["clock"].filter_by_commitment.unwrap());
        let with_fees = !first.transactions.is_empty();
        if with_fees {
            assert_eq!(first.transactions["priority_fees"].vote, Some(false));
            assert_eq!(first.transactions["priority_fees"].failed, Some(false));
            assert_eq!(
                first.transactions["priority_fees"].account_include,
                [COMPUTE]
            );
            assert_eq!(
                first.transactions["priority_fees"]
                    .account_required
                    .as_slice(),
                std::slice::from_ref(&self.program)
            );
            if self.reject_fees {
                return Err(Status::permission_denied("account subscriptions only"));
            }
        }
        let (tx, rx) = mpsc::channel(8);
        let ping = self.ping.clone();
        let late_failure = self.late_failure;
        tokio::spawn(async move {
            let mut updates = vec![
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
                UpdateOneof::Account(SubscribeUpdateAccount {
                    slot: 102,
                    account: Some(SubscribeUpdateAccountInfo {
                        pubkey: vec![4; 32],
                        owner: vec![0; 32],
                        lamports: 0,
                        write_version: 10,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                UpdateOneof::Ping(SubscribeUpdatePing {}),
            ];
            if with_fees {
                updates.extend(
                    (0..8).map(|index| {
                        UpdateOneof::Transaction(sample(index, 200_000, index % 2 == 0))
                    }),
                );
            }
            for update in updates {
                if tx
                    .send(Ok(SubscribeUpdate {
                        update_oneof: Some(update),
                        created_at: Some(yellowstone_grpc_proto::prost_types::Timestamp {
                            seconds: crate::now() as i64,
                            nanos: 0,
                        }),
                        ..Default::default()
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            if late_failure {
                let _ = tx
                    .send(Err(Status::resource_exhausted(
                        "synthetic late stream failure",
                    )))
                    .await;
                return;
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
    for (token, reject_fees) in [
        (None, false),
        (Some("local-test-token".to_owned()), false),
        (None, true),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ping = Arc::new(AtomicBool::new(false));
        let program = Pubkey::new_from_array([1; 32]);
        let service = Mock {
            token: token.clone(),
            ping: ping.clone(),
            program: program.to_string(),
            reject_fees,
            late_failure: false,
            subscriptions: Arc::default(),
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
        let config = test_config(address, token);
        let (tx, mut rx) = mpsc::channel(8);
        let fees = Fees::default();
        let client = tokio::spawn(connect(config, tx, fees.clone()));
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
            let Some(Event::Accounts(closed)) = rx.recv().await else {
                panic!("missing list close")
            };
            assert_eq!(closed[0].0, Pubkey::new_from_array([4; 32]).to_string());
            assert!(closed[0].1.data.is_empty());
            assert_eq!(closed[0].1.lamports, 0);
            while !ping.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if !reject_fees {
                while fees.snapshot()["baselineMicroLamports"] != 200_000 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } else {
                assert_eq!(fees.snapshot()["baselineMicroLamports"], 0);
            }
            assert!(rx.try_recv().is_err());
        })
        .await
        .unwrap();
        client.abort();
        server.abort();
        let _ = client.await;
        let _ = server.await;
    }
}

fn sample(index: u8, price: u64, loaded: bool) -> SubscribeUpdateTransaction {
    let compute = COMPUTE_KEY.to_vec();
    SubscribeUpdateTransaction {
        slot: 100 + u64::from(index) / 4,
        transaction: Some(SubscribeUpdateTransactionInfo {
            signature: vec![index; 64],
            transaction: Some(Transaction {
                message: Some(Message {
                    account_keys: if loaded {
                        vec![vec![4; 32]]
                    } else {
                        vec![compute.clone()]
                    },
                    instructions: vec![CompiledInstruction {
                        program_id_index: u32::from(loaded),
                        data: [&[3][..], &price.to_le_bytes()].concat(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            meta: Some(TransactionStatusMeta {
                loaded_readonly_addresses: if loaded { vec![compute] } else { vec![] },
                ..Default::default()
            }),
            ..Default::default()
        }),
    }
}

#[test]
fn priority_parser_accepts_native_prices_and_loaded_addresses_without_using_total_fees() {
    for loaded in [false, true] {
        let mut update = sample(1, 312_567, loaded);
        update
            .transaction
            .as_mut()
            .unwrap()
            .meta
            .as_mut()
            .unwrap()
            .fee = 9_000_000;
        assert_eq!(priority(update), Some((100, [1; 64], 312_567)));
    }
    let mut missing = sample(2, 0, false);
    missing
        .transaction
        .as_mut()
        .unwrap()
        .transaction
        .as_mut()
        .unwrap()
        .message
        .as_mut()
        .unwrap()
        .instructions
        .clear();
    assert_eq!(priority(missing), Some((100, [2; 64], 0)));
}

#[test]
fn priority_parser_ignores_failed_votes_bad_keys_and_unsupported_or_ambiguous_budgets() {
    for case in 0..8 {
        let mut update = sample(1, 312_567, false);
        let info = update.transaction.as_mut().unwrap();
        match case {
            0 => info.is_vote = true,
            1 => info.meta.as_mut().unwrap().err = Some(TransactionError::default()),
            2 => {
                info.signature.truncate(63);
            }
            _ => {
                let message = info.transaction.as_mut().unwrap().message.as_mut().unwrap();
                match case {
                    3 => {
                        message.account_keys[0].truncate(31);
                    }
                    4 => message.config = Some(TransactionConfig::default()),
                    5 => message.instructions.push(message.instructions[0].clone()),
                    6 => {
                        message.instructions[0].data.push(0);
                    }
                    7 => {
                        message.instructions[0].accounts.push(0);
                    }
                    _ => unreachable!(),
                }
            }
        }
        assert!(priority(update).is_none(), "case {case}");
    }
}

fn test_config(address: std::net::SocketAddr, token: Option<String>) -> Config {
    Config {
        rpc_url: "http://127.0.0.1:9".into(),
        ws_url: None,
        grpc_url: Some(format!("http://{address}")),
        grpc_token: token,
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1:8790".into(),
        database: "unused".into(),
        program: Pubkey::new_from_array([1; 32]),
        market: Pubkey::new_from_array([2; 32]),
        provider_list: Pubkey::new_from_array([4; 32]),
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
        provider_limit_lookup_tables: vec![],
    }
}

#[tokio::test]
async fn late_subscription_errors_require_session_reconciliation_instead_of_silent_resubscribe() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let subscriptions = Arc::new(AtomicUsize::new(0));
    let service = Mock {
        token: None,
        ping: Arc::default(),
        program: Pubkey::new_from_array([1; 32]).to_string(),
        reject_fees: false,
        late_failure: true,
        subscriptions: subscriptions.clone(),
    };
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(geyser_server::GeyserServer::new(service))
            .serve_with_incoming(stream::unfold(listener, |listener| async {
                Some((listener.accept().await.map(|(socket, _)| socket), listener))
            }))
            .await
            .unwrap();
    });
    let (tx, mut rx) = mpsc::channel(8);
    let client = tokio::spawn(connect(test_config(address, None), tx, Fees::default()));
    tokio::time::timeout(Duration::from_secs(3), async {
        assert!(matches!(rx.recv().await, Some(Event::Connected)));
        assert!(matches!(rx.recv().await, Some(Event::Activity(101))));
        assert!(client.await.unwrap().is_err());
        assert_eq!(subscriptions.load(Ordering::SeqCst), 1);
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}
