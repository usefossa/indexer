use crate::error::{Error, Result};
use solana_pubkey::Pubkey;
use std::{env, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

#[derive(Clone)]
pub struct Config {
    pub rpc_url: String,
    pub ws_url: Option<String>,
    pub grpc_url: Option<String>,
    pub grpc_token: Option<String>,
    pub bind: SocketAddr,
    pub public_url: String,
    pub database: PathBuf,
    pub program: Pubkey,
    pub market: Pubkey,
    pub mint: Pubkey,
    pub genesis: String,
    pub commitment: String,
    pub reconcile: Duration,
    pub stale: Duration,
    pub max_accounts: usize,
    pub max_rpc_bytes: usize,
    pub max_connections: usize,
    pub request_limit: usize,
    pub auth_limit: usize,
    pub access_secs: u64,
    pub refresh_secs: u64,
}
impl Config {
    pub fn from_env() -> Result<Self> {
        let get = |name: &str, default: &str| env::var(name).unwrap_or_else(|_| default.into());
        let key = |name, default| {
            Pubkey::from_str(&get(name, default))
                .map_err(|_| Error::bad("Invalid configured public key"))
        };
        let number = |name, default| {
            get(name, default)
                .parse::<u64>()
                .map_err(|_| Error::bad("Invalid numeric setting"))
        };
        let optional = |name| env::var(name).ok().filter(|v| !v.is_empty());
        let config = Self {
            rpc_url: get("FOSSA_RPC_URL", ""),
            ws_url: optional("FOSSA_WS_URL"),
            grpc_url: optional("FOSSA_GRPC_URL"),
            grpc_token: optional("FOSSA_GRPC_TOKEN"),
            bind: get("FOSSA_BIND", "127.0.0.1:8790")
                .parse()
                .map_err(|_| Error::bad("Invalid bind address"))?,
            public_url: get("FOSSA_PUBLIC_URL", "http://127.0.0.1:8790")
                .trim_end_matches('/')
                .into(),
            database: get("FOSSA_DATABASE", "state/indexer.sqlite").into(),
            program: key(
                "FOSSA_PROGRAM_ID",
                "F8q85nw6UScZCBKBJqVAu7ZdYhmT9XRsm6TqZzJqXSNT",
            )?,
            market: key(
                "FOSSA_MARKET",
                "8wceBTFDAiC9WiT3xZGRf9Nf4xrz8eBCxExBtDWz1Som",
            )?,
            mint: key(
                "FOSSA_USDC_MINT",
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            )?,
            genesis: get(
                "FOSSA_GENESIS_HASH",
                "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
            ),
            commitment: get("FOSSA_COMMITMENT", "confirmed"),
            reconcile: Duration::from_secs(number("FOSSA_RECONCILE_SECS", "30")?),
            stale: Duration::from_secs(number("FOSSA_STALE_SECS", "45")?),
            max_accounts: number("FOSSA_MAX_ACCOUNTS", "100000")? as usize,
            max_rpc_bytes: number("FOSSA_MAX_RPC_BYTES", "134217728")? as usize,
            max_connections: number("FOSSA_MAX_CONNECTIONS", "128")? as usize,
            request_limit: number("FOSSA_REQUESTS_PER_MINUTE", "600")? as usize,
            auth_limit: number("FOSSA_AUTH_PER_MINUTE", "30")? as usize,
            access_secs: number("FOSSA_ACCESS_SECS", "3600")?,
            refresh_secs: number("FOSSA_REFRESH_SECS", "2592000")?,
        };
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        endpoint(&self.rpc_url, &["http", "https"], false)?;
        endpoint(&self.public_url, &["http", "https"], true)?;
        if let Some(url) = &self.ws_url {
            endpoint(url, &["ws", "wss"], false)?;
        }
        if let Some(url) = &self.grpc_url {
            endpoint(url, &["http", "https"], false)?;
        }
        if self.ws_url.is_none() && self.grpc_url.is_none() {
            return Err(Error::bad("Set FOSSA_WS_URL or FOSSA_GRPC_URL"));
        }
        if !matches!(self.commitment.as_str(), "confirmed" | "finalized")
            || self.genesis.is_empty()
            || self.reconcile < Duration::from_secs(1)
            || self.stale < Duration::from_secs(3)
            || self.reconcile >= self.stale
            || !(1..=1_000_000).contains(&self.max_accounts)
            || !(1_048_576..=1_073_741_824).contains(&self.max_rpc_bytes)
            || !(1..=4096).contains(&self.max_connections)
            || !(1..=100_000).contains(&self.request_limit)
            || !(1..=1000).contains(&self.auth_limit)
            || !(30..=86400).contains(&self.access_secs)
            || !(60..=31_536_000).contains(&self.refresh_secs)
        {
            return Err(Error::bad("Invalid configuration limits or commitment"));
        }
        Ok(())
    }
}
fn endpoint(value: &str, schemes: &[&str], public: bool) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| Error::bad("Invalid configured endpoint"))?;
    if !schemes.contains(&url.scheme())
        || url.host_str().is_none()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || public
            && (url.query().is_some()
                || url.path() != "/"
                || url.scheme() == "http"
                    && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err(Error::bad(
            "Invalid configured endpoint; public URL requires HTTPS except loopback",
        ));
    }
    Ok(())
}
