use crate::{
    App,
    error::{Error, Result},
    protocol,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use rand::RngCore;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Default)]
pub struct Connections(Mutex<HashMap<String, usize>>);
pub struct Connection {
    limits: Arc<Connections>,
    keys: Vec<String>,
}
impl Connections {
    pub fn enter(self: &Arc<Self>, wallet: &str, peer: Option<IpAddr>) -> Result<Connection> {
        let mut keys = vec![(format!("wallet:{wallet}"), 8)];
        if let Some(peer) = peer.filter(|ip| !ip.is_loopback()) {
            keys.push((format!("peer:{peer}"), 32));
        }
        let mut counts = self.0.lock().map_err(|_| Error::internal())?;
        if keys
            .iter()
            .any(|(key, limit)| counts.get(key).copied().unwrap_or(0) >= *limit)
        {
            return Err(Error(
                StatusCode::TOO_MANY_REQUESTS,
                "Live connection limit reached",
            ));
        }
        let keys = keys
            .into_iter()
            .map(|(key, _)| {
                *counts.entry(key.clone()).or_default() += 1;
                key
            })
            .collect();
        Ok(Connection {
            limits: self.clone(),
            keys,
        })
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.limits.0.lock() {
            for key in &self.keys {
                if let Some(count) = counts.get_mut(key) {
                    *count -= 1;
                    if *count == 0 {
                        counts.remove(key);
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Limits(Mutex<HashMap<String, (Instant, usize)>>);
impl Limits {
    pub fn check(&self, key: String, max: usize) -> Result<()> {
        let mut limits = self.0.lock().map_err(|_| Error::internal())?;
        if limits.len() >= 8192 {
            limits.retain(|_, (at, _)| at.elapsed().as_secs() < 60);
            if !limits.contains_key(&key) && limits.len() >= 8192 {
                return Err(Error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Request limit reached",
                ));
            }
        }
        let (at, count) = limits.entry(key).or_insert((Instant::now(), 0));
        if at.elapsed().as_secs() >= 60 {
            *at = Instant::now();
            *count = 0;
        }
        if *count >= max {
            return Err(Error(
                StatusCode::TOO_MANY_REQUESTS,
                "Request limit reached",
            ));
        }
        *count += 1;
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub wallet: String,
    pub lane: String,
    pub family: String,
    pub expires: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub wallet: String,
    pub lane: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub challenge: String,
    pub signature: String,
}
pub fn token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn bearer(headers: &HeaderMap) -> Result<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(Error::unauthorized)
}
pub async fn session(app: &App, headers: &HeaderMap) -> Result<Session> {
    let hash = digest(bearer(headers)?);
    let now = crate::now();
    let session=app.db.call(move|conn|conn.query_row("SELECT wallet,lane,family,expires FROM sessions WHERE hash=?1 AND kind=0 AND expires>?2",params![hash,now],|r|Ok(Session{wallet:r.get(0)?,lane:r.get(1)?,family:r.get(2)?,expires:r.get(3)?})).optional()?.ok_or_else(Error::unauthorized)).await?;
    Ok(session)
}
fn issue(
    conn: &rusqlite::Transaction<'_>,
    app: &crate::config::Config,
    wallet: &str,
    lane: &str,
    family: &str,
) -> Result<Value> {
    let access = token();
    let refresh = token();
    let now = crate::now();
    let expires = now + app.access_secs;
    conn.execute("DELETE FROM sessions WHERE expires<=?1", [now])?;
    let count: u64 = conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
    if count >= 100_000 {
        return Err(Error::unavailable("Session capacity reached"));
    }
    for (raw, kind, expires) in [(&access, 0, expires), (&refresh, 1, now + app.refresh_secs)] {
        conn.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6)",
            params![digest(raw), family, kind, wallet, lane, expires],
        )?;
    }
    Ok(
        json!({"wallet":wallet,"lane":lane,"access":access,"refresh":refresh,"expires":expires*1000}),
    )
}
pub async fn challenge(
    State(app): State<App>,
    Json(identity): Json<Identity>,
) -> Result<Json<Value>> {
    protocol::pubkey(&identity.wallet)?;
    protocol::pubkey(&identity.lane)?;
    app.limits
        .check(format!("wallet:{}", identity.wallet), app.config.auth_limit)?;
    let challenge = token();
    let message = format!(
        "fossa-opencode-connect-v1:{}:{}:{}:{}:{}",
        app.config.program, app.config.market, identity.wallet, identity.lane, challenge
    );
    let hash = digest(&challenge);
    let saved = message.clone();
    let now = crate::now();
    app.db
        .call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM challenges WHERE expires<=?1", [now])?;
            let count: u64 = tx.query_row("SELECT count(*) FROM challenges", [], |r| r.get(0))?;
            if count >= 10_000 {
                return Err(Error::unavailable("Challenge capacity reached"));
            }
            tx.execute(
                "INSERT INTO challenges VALUES(?1,?2,?3,?4,?5)",
                params![hash, identity.wallet, identity.lane, saved, now + 120],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"challenge":challenge,"message":message})))
}
pub async fn connect(State(app): State<App>, Json(proof): Json<Proof>) -> Result<Json<Value>> {
    if proof.challenge.len() != 64
        || !proof.challenge.bytes().all(|v| v.is_ascii_hexdigit())
        || proof.signature.len() > 128
    {
        return Err(Error::bad("Invalid wallet proof"));
    }
    let signature = STANDARD
        .decode(&proof.signature)
        .ok()
        .and_then(|s| Signature::from_slice(&s).ok())
        .ok_or(Error::bad("Invalid wallet signature"))?;
    let hash = digest(&proof.challenge);
    let now = crate::now();
    let identity = app
        .db
        .call({
            let hash = hash.clone();
            move |conn| {
                conn.query_row(
                    "SELECT wallet,lane,message FROM challenges WHERE hash=?1 AND expires>?2",
                    params![hash, now],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(Error::bad("Wallet proof expired or already used"))
            }
        })
        .await?;
    let owner = protocol::pubkey(&identity.0)?;
    VerifyingKey::from_bytes(&owner.to_bytes())
        .and_then(|k| k.verify_strict(identity.2.as_bytes(), &signature))
        .map_err(|_| Error(StatusCode::FORBIDDEN, "Invalid wallet signature"))?;
    let lane = app
        .rpc
        .call(
            "getAccountInfo",
            json!([identity.1,{"encoding":"base64","commitment":app.config.commitment}]),
        )
        .await?;
    if !lane["value"].is_null() {
        let bytes =
            protocol::bytes(&lane["value"]).ok_or(Error::bad("Invalid existing user account"))?;
        if lane["value"]["owner"] != app.config.program.to_string()
            || protocol::validate(&identity.1, &bytes, &app.config) != Some(protocol::Kind::User)
            || protocol::address(&bytes, 8) != identity.0
        {
            return Err(Error(
                StatusCode::FORBIDDEN,
                "User account belongs to another wallet or protocol",
            ));
        }
    }
    let config = app.config.clone();
    Ok(Json(
        app.db
            .call(move |conn| {
                let tx = conn.transaction()?;
                if tx.execute(
                    "DELETE FROM challenges WHERE hash=?1 AND expires>?2",
                    params![hash, crate::now()],
                )? != 1
                {
                    return Err(Error::bad("Wallet proof expired or already used"));
                }
                let value = issue(&tx, &config, &identity.0, &identity.1, &token())?;
                tx.commit()?;
                Ok(value)
            })
            .await?,
    ))
}
pub async fn refresh(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let hash = digest(bearer(&headers)?);
    let config = app.config.clone();
    Ok(Json(app.db.call(move|conn|{let tx=conn.transaction()?;let (wallet,lane,family)=tx.query_row("SELECT wallet,lane,family FROM sessions WHERE hash=?1 AND kind=1 AND expires>?2",params![hash,crate::now()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?.ok_or_else(Error::unauthorized)?;tx.execute("DELETE FROM sessions WHERE family=?1",[&family])?;let value=issue(&tx,&config,&wallet,&lane,&family)?;tx.commit()?;Ok(value)}).await?))
}
pub async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let hash = digest(bearer(&headers)?);
    app.db
        .call(move |conn| {
            conn.execute(
                "DELETE FROM sessions WHERE family IN (SELECT family FROM sessions WHERE hash=?1)",
                [hash],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok":true})))
}
pub async fn device() -> Error {
    Error(
        StatusCode::NOT_IMPLEMENTED,
        "Browser quick connect is unavailable on this self-hosted indexer. Choose Import wallet in OpenCode; keys stay on your computer.",
    )
}
