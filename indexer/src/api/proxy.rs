use crate::{
    App, auth,
    error::{Error, Result},
    protocol,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bincode::Options;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::Value;
use solana_transaction::Transaction;

pub fn validate_transaction(raw: &str, wallet: &str, program: &str) -> Result<()> {
    if raw.len() > 1644 {
        return Err(Error::bad("Transaction exceeds packet limit"));
    }
    let bytes = STANDARD
        .decode(raw)
        .map_err(|_| Error::bad("Invalid transaction encoding"))?;
    if bytes.len() > 1232 {
        return Err(Error::bad("Transaction exceeds packet limit"));
    }
    let tx: Transaction = bincode::options()
        .with_fixint_encoding()
        .with_limit(1232)
        .reject_trailing_bytes()
        .deserialize(&bytes)
        .map_err(|_| Error::bad("Only signed legacy transactions are supported"))?;
    let count = usize::from(tx.message.header.num_required_signatures);
    let keys = &tx.message.account_keys;
    if count == 0
        || tx.signatures.len() != count
        || keys.len() < count
        || usize::from(tx.message.header.num_readonly_signed_accounts) >= count
        || usize::from(tx.message.header.num_readonly_unsigned_accounts) > keys.len() - count
        || keys[0].to_string() != wallet
    {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "Transaction payer must be authenticated wallet",
        ));
    }
    let message =
        bincode::serialize(&tx.message).map_err(|_| Error::bad("Invalid transaction message"))?;
    for (signature, key) in tx.signatures.iter().zip(&keys[..count]) {
        let signature = Signature::from_slice(signature.as_ref())
            .map_err(|_| Error::bad("Invalid signature size"))?;
        VerifyingKey::from_bytes(&key.to_bytes())
            .and_then(|k| k.verify_strict(&message, &signature))
            .map_err(|_| Error::bad("Invalid transaction signature"))?;
    }
    for instruction in &tx.message.instructions {
        let id = keys
            .get(instruction.program_id_index as usize)
            .ok_or(Error::bad("Invalid instruction program"))?
            .to_string();
        if ![
            program,
            "11111111111111111111111111111111",
            "ComputeBudget111111111111111111111111111111",
            protocol::TOKEN,
            protocol::ATA,
        ]
        .contains(&id.as_str())
            || instruction
                .accounts
                .iter()
                .any(|i| *i as usize >= keys.len())
        {
            return Err(Error(
                StatusCode::FORBIDDEN,
                "Instruction program not allowed",
            ));
        }
    }
    Ok(())
}
pub async fn rpc(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let session = auth::session(&app, &headers).await?;
    let method = body["method"]
        .as_str()
        .ok_or(Error::bad("Missing RPC method"))?;
    if !matches!(
        method,
        "getAccountInfo"
            | "getMultipleAccounts"
            | "getLatestBlockhash"
            | "getSignatureStatuses"
            | "getBlockHeight"
            | "getMinimumBalanceForRentExemption"
            | "getFeeForMessage"
            | "getGenesisHash"
            | "getSlot"
            | "getBalance"
            | "getTokenAccountBalance"
            | "getHealth"
            | "sendTransaction"
    ) {
        return Err(Error(StatusCode::FORBIDDEN, "RPC method not allowed"));
    }
    if body["jsonrpc"] != "2.0"
        || !body["params"].is_array()
        || body.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !["jsonrpc", "id", "method", "params"].contains(&k.as_str()))
        })
        || body["params"].as_array().is_some_and(|p| p.len() > 3)
    {
        return Err(Error::bad("Invalid RPC request"));
    }
    if method == "getMultipleAccounts" || method == "getSignatureStatuses" {
        let maximum = if method == "getMultipleAccounts" {
            100
        } else {
            256
        };
        if body["params"][0]
            .as_array()
            .is_none_or(|a| a.len() > maximum || a.iter().any(|v| !v.is_string()))
        {
            return Err(Error::bad("Invalid RPC account or signature list"));
        }
    }
    if method == "sendTransaction" {
        if !app.index.ready(&app.config).await {
            return Err(Error::unavailable(
                "Indexer synchronizing; transaction not forwarded",
            ));
        }
        let raw = body["params"][0]
            .as_str()
            .ok_or(Error::bad("Missing signed transaction"))?;
        if body["params"][1]["encoding"] != "base64" {
            return Err(Error::bad("Use base64 transaction encoding"));
        }
        validate_transaction(raw, &session.wallet, &app.config.program.to_string())?;
    }
    Ok(Json(app.rpc.raw(&body, false).await?))
}
