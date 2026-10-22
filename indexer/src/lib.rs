pub mod api;
pub mod auth;
pub mod catalog;
pub mod chain;
pub mod config;
pub mod db;
pub mod error;
pub mod index;
pub mod metadata;
pub mod protocol;
pub mod receipts;
pub mod rpc;

use crate::{config::Config, db::Db, error::Result, index::Index, rpc::Rpc};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Semaphore, watch};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone)]
pub struct App(pub Arc<Inner>);
pub struct Inner {
    pub config: Config,
    pub db: Db,
    pub index: Index,
    pub rpc: Rpc,
    pub sockets: Arc<Semaphore>,
    pub http_requests: Arc<Semaphore>,
    pub limits: auth::Limits,
    pub chain: watch::Sender<Option<Arc<serde_json::Value>>>,
    pub shutdown: watch::Sender<bool>,
}
impl std::ops::Deref for App {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}
impl App {
    pub async fn new(config: Config) -> Result<Self> {
        config.validate()?;
        let db = Db::open(&config).await?;
        let index = Index::restore(&db, &config).await?;
        let rpc = Rpc::new(&config)?;
        let sockets = Arc::new(Semaphore::new(config.max_connections));
        Ok(Self(Arc::new(Inner {
            config,
            db,
            index,
            rpc,
            sockets,
            http_requests: Arc::new(Semaphore::new(256)),
            limits: auth::Limits::default(),
            chain: watch::channel(None).0,
            shutdown: watch::channel(false).0,
        })))
    }
    pub fn start(&self) -> Vec<tokio::task::JoinHandle<()>> {
        vec![
            tokio::spawn(chain::run(self.clone())),
            tokio::spawn(metadata::run(self.clone())),
            tokio::spawn(receipts::run(self.clone())),
            tokio::spawn(api::wallet::quotes(self.clone())),
        ]
    }
}
