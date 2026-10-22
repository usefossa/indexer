use crate::{
    config::Config,
    error::{Error, Result},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct Rpc {
    client: reqwest::Client,
    url: Arc<str>,
    permits: Arc<Semaphore>,
    max_bytes: usize,
}
impl Rpc {
    pub fn new(config: &Config) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(16)
            .build()
            .map_err(|_| Error::unavailable("HTTP client unavailable"))?;
        Ok(Self {
            client,
            url: config.rpc_url.clone().into(),
            permits: Arc::new(Semaphore::new(32)),
            max_bytes: config.max_rpc_bytes,
        })
    }
    pub async fn raw(&self, body: &Value, bulk: bool) -> Result<Value> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| Error::unavailable("RPC stopped"))?;
        let response = self
            .client
            .post(self.url.as_ref())
            .json(body)
            .send()
            .await
            .map_err(|_| Error::unavailable("Solana RPC unreachable"))?
            .error_for_status()
            .map_err(|_| Error::unavailable("Solana RPC rejected request"))?;
        let max = if bulk {
            self.max_bytes
        } else {
            8 * 1024 * 1024
        };
        let bytes = limited(response, max).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::unavailable("Invalid Solana RPC response"))
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let value = self
            .raw(
                &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
                method == "getProgramAccounts",
            )
            .await?;
        if value.get("error").is_some() {
            return Err(Error::unavailable("Solana RPC returned an error"));
        }
        value
            .get("result")
            .cloned()
            .ok_or(Error::unavailable("Incomplete Solana RPC response"))
    }
}
pub async fn limited(response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(Error::unavailable(
            "Upstream response exceeds configured limit",
        ));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::unavailable("Truncated upstream response"))?;
        if chunk.len() > max.saturating_sub(body.len()) {
            return Err(Error::unavailable(
                "Upstream response exceeds configured limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
