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
use solana_transaction::versioned::VersionedTransaction;
use std::collections::HashSet;

fn decode_transaction(raw: &str) -> Result<VersionedTransaction> {
    if raw.len() > 1644 {
        return Err(Error::bad("Transaction exceeds packet limit"));
    }
    let bytes = STANDARD
        .decode(raw)
        .map_err(|_| Error::bad("Invalid transaction encoding"))?;
    if bytes.len() > 1232 {
        return Err(Error::bad("Transaction exceeds packet limit"));
    }
    let tx: VersionedTransaction = bincode::options()
        .with_fixint_encoding()
        .with_limit(1232)
        .reject_trailing_bytes()
        .deserialize(&bytes)
        .map_err(|_| Error::bad("Invalid legacy or v0 transaction"))?;
    tx.sanitize()
        .map_err(|_| Error::bad("Invalid transaction message"))?;
    Ok(tx)
}

fn validate_message(
    tx: &VersionedTransaction,
    wallet: &str,
    program: &str,
    unsigned: bool,
) -> Result<()> {
    let count = usize::from(tx.message.header().num_required_signatures);
    let keys = tx.message.static_account_keys();
    if keys.first().is_none_or(|key| key.to_string() != wallet) {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "Transaction payer must be authenticated wallet",
        ));
    }
    let mut unique = HashSet::new();
    if keys.iter().any(|key| !unique.insert(key)) {
        return Err(Error::bad("Duplicate static transaction account"));
    }
    let mut tables = HashSet::new();
    for lookup in tx.message.address_table_lookups().unwrap_or_default() {
        let mut indexes = HashSet::new();
        if !tables.insert(lookup.account_key)
            || lookup
                .writable_indexes
                .iter()
                .chain(&lookup.readonly_indexes)
                .any(|index| !indexes.insert(index))
        {
            return Err(Error::bad("Duplicate lookup table or account index"));
        }
    }
    let message =
        bincode::serialize(&tx.message).map_err(|_| Error::bad("Invalid transaction message"))?;
    for (signature, key) in tx.signatures.iter().zip(&keys[..count]) {
        if unsigned {
            if signature.as_ref().iter().any(|byte| *byte != 0) {
                return Err(Error::bad("Unsigned simulation requires empty signatures"));
            }
            continue;
        }
        let signature = Signature::from_slice(signature.as_ref())
            .map_err(|_| Error::bad("Invalid signature size"))?;
        VerifyingKey::from_bytes(&key.to_bytes())
            .and_then(|k| k.verify_strict(&message, &signature))
            .map_err(|_| Error::bad("Invalid transaction signature"))?;
    }
    for instruction in tx.message.instructions() {
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
        {
            return Err(Error(
                StatusCode::FORBIDDEN,
                "Instruction program not allowed",
            ));
        }
    }
    Ok(())
}

pub fn validate_transaction(raw: &str, wallet: &str, program: &str) -> Result<()> {
    validate_message(&decode_transaction(raw)?, wallet, program, false)
}

fn validate_simulation(raw: &str, wallet: &str, program: &str, options: &Value) -> Result<()> {
    let options = options
        .as_object()
        .ok_or(Error::bad("Invalid simulation options"))?;
    if options.keys().any(|key| {
        ![
            "encoding",
            "sigVerify",
            "commitment",
            "minContextSlot",
            "replaceRecentBlockhash",
        ]
        .contains(&key.as_str())
    }) || options.get("commitment").is_some_and(|value| {
        !matches!(
            value.as_str(),
            Some("processed" | "confirmed" | "finalized")
        )
    }) || options
        .get("minContextSlot")
        .is_some_and(|value| value.as_u64().is_none())
        || options
            .get("replaceRecentBlockhash")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err(Error::bad("Invalid simulation options"));
    }
    let unsigned = match options.get("sigVerify").and_then(Value::as_bool) {
        Some(false) => true,
        Some(true) if options.get("replaceRecentBlockhash") != Some(&Value::Bool(true)) => false,
        _ => return Err(Error::bad("Simulation requires explicit signature policy")),
    };
    validate_message(&decode_transaction(raw)?, wallet, program, unsigned)
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
            | "isBlockhashValid"
            | "getSignatureStatuses"
            | "getBlockHeight"
            | "getEpochInfo"
            | "getMinimumBalanceForRentExemption"
            | "getFeeForMessage"
            | "getGenesisHash"
            | "getSlot"
            | "getBalance"
            | "getTokenAccountBalance"
            | "getHealth"
            | "simulateTransaction"
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
    if method == "isBlockhashValid" {
        let params = body["params"].as_array().expect("validated params");
        if !(1..=2).contains(&params.len())
            || params[0]
                .as_str()
                .filter(|hash| hash.len() <= 44)
                .and_then(|hash| protocol::pubkey(hash).ok())
                .is_none()
            || params.get(1).is_some_and(|options| {
                options.as_object().is_none_or(|options| {
                    options.iter().any(|(key, value)| match key.as_str() {
                        "commitment" => !matches!(
                            value.as_str(),
                            Some("processed" | "confirmed" | "finalized")
                        ),
                        "minContextSlot" => value.as_u64().is_none(),
                        _ => true,
                    })
                })
            })
        {
            return Err(Error::bad("Invalid blockhash lookup"));
        }
    }
    if method == "getEpochInfo" {
        let params = body["params"].as_array().expect("validated params");
        if params.len() > 1
            || params.first().is_some_and(|options| {
                options.as_object().is_none_or(|options| {
                    options.iter().any(|(key, value)| match key.as_str() {
                        "commitment" => !matches!(
                            value.as_str(),
                            Some("processed" | "confirmed" | "finalized")
                        ),
                        "minContextSlot" => value.as_u64().is_none(),
                        _ => true,
                    })
                })
            })
        {
            return Err(Error::bad("Invalid epoch lookup"));
        }
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
    if matches!(method, "sendTransaction" | "simulateTransaction") {
        if !app.index.ready(&app.config).await {
            return Err(Error::unavailable(
                "Indexer synchronizing; transaction not forwarded",
            ));
        }
        let raw = body["params"][0]
            .as_str()
            .ok_or(Error::bad("Missing transaction"))?;
        if body["params"][1]["encoding"] != "base64" {
            return Err(Error::bad("Use base64 transaction encoding"));
        }
        if method == "sendTransaction" {
            validate_transaction(raw, &session.wallet, &app.config.program.to_string())?;
        } else {
            validate_simulation(
                raw,
                &session.wallet,
                &app.config.program.to_string(),
                &body["params"][1],
            )?;
        }
    }
    Ok(Json(app.rpc.raw(&body, false).await?))
}
