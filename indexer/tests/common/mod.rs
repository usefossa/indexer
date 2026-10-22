use fossa_indexer::config::Config;
use solana_pubkey::Pubkey;
use std::{path::Path, time::Duration};

pub fn config(path: &Path) -> Config {
    Config {
        rpc_url: "http://127.0.0.1:9".into(),
        ws_url: Some("ws://127.0.0.1:9".into()),
        grpc_url: None,
        grpc_token: None,
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1:8790".into(),
        database: path.join("indexer.sqlite"),
        program: Pubkey::new_from_array([1; 32]),
        market: Pubkey::new_from_array([2; 32]),
        mint: Pubkey::new_from_array([3; 32]),
        genesis: "mock-genesis".into(),
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
    }
}
