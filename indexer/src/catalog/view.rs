use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

// retain protocol identity and fee terms in both views so clients can bind a book to its market.
pub(crate) fn project(catalog: &Value, model: Option<&str>) -> Value {
    let mut result = serde_json::Map::new();
    for key in [
        "ready",
        "slot",
        "programId",
        "market",
        "providerList",
        "providerListFeeBps",
        "providerListProtocolShareBps",
        "listRouting",
        "listRoutingEnabled",
        "providerLimitsEnabled",
        "providerLimitLookupTables",
    ] {
        if let Some(value) = catalog.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    let models = catalog["models"].as_array().into_iter().flatten();
    if let Some(id) = model {
        result.insert(
            "models".into(),
            Value::Array(
                models
                    .filter(|m| m["id"].as_str() == Some(id))
                    .cloned()
                    .collect(),
            ),
        );
    } else {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        result.insert("aliases".into(), catalog["aliases"].clone());
        result.insert("directory".into(), Value::Bool(true));
        result.insert(
            "models".into(),
            Value::Array(models.filter_map(|model| summary(model, now)).collect()),
        );
    }
    Value::Object(result)
}

fn available_quota(provider: &Value, now: u64) -> bool {
    let limits = &provider["providerLimits"];
    if limits.is_null() {
        return true;
    }
    match (
        limits["global"]["quota"].as_u64(),
        provider["quotaWindowEnd"].as_u64(),
        provider["quotaUsed"].as_u64(),
    ) {
        (Some(0), _, _) => true,
        (Some(quota), Some(end), Some(used)) => now >= end || used < quota,
        _ => false,
    }
}

fn summary(model: &Value, now: u64) -> Option<Value> {
    let offers = model["offers"].as_array()?;
    let active = || offers.iter().filter(|offer| offer["active"] == true);
    let latest = active().max_by_key(|offer| offer["checkpointOrder"].as_u64());
    let latest = latest
        .and_then(|offer| offer["checkpointOrder"].as_u64().map(|_| offer))
        .or_else(|| active().next())?;
    let number = |offer: &Value, field: &str| offer[field].as_u64().unwrap_or(0);
    let has = |offer: &Value, side: &str, kind: &str| {
        offer["modalities"][side]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == kind))
    };
    let allow_unordered = active().all(|offer| {
        offer["checkpointOrder"].is_null() || offer["checkpoint"] == latest["checkpoint"]
    });
    let mut context = 0;
    let mut output = 0;
    let mut image = false;
    let mut prices = Vec::new();
    for offer in active().filter(|offer| {
        offer["checkpoint"] == latest["checkpoint"]
            || allow_unordered && offer["checkpointOrder"].is_null()
    }) {
        let provider = &offer["provider"];
        if provider["bookingLimits"]["updating"] == true
            || !has(offer, "input", "text")
            || !has(offer, "output", "text")
        {
            continue;
        }
        context = context.max(number(offer, "contextTokens"));
        output = output.max(number(offer, "outputTokens"));
        image |= has(offer, "input", "image") && number(offer, "maxImageTokens") > 0;
        if offer["available"] == true
            && provider["endpoint"]
                .as_str()
                .is_some_and(|url| !url.is_empty())
            && available_quota(provider, now)
        {
            let cost = number(offer, "inputRate")
                + number(offer, "cachedRate")
                + number(offer, "outputRate");
            prices.push((offer, cost, number(offer, "speed")));
        }
    }
    if (context == 0 || output == 0)
        && !active().any(|offer| has(offer, "input", "text") && has(offer, "output", "text"))
    {
        return None;
    }
    let min_cost = prices.iter().map(|p| p.1).min().unwrap_or(0);
    let max_cost = prices.iter().map(|p| p.1).max().unwrap_or(0);
    let min_speed = prices.iter().map(|p| p.2).min().unwrap_or(0);
    let max_speed = prices.iter().map(|p| p.2).max().unwrap_or(0);
    let price = prices.iter().min_by_key(|(offer, cost, speed)| {
        let c = (cost - min_cost) as f64 / (max_cost - min_cost).max(1) as f64;
        let s = (max_speed - speed) as f64 / (max_speed - min_speed).max(1) as f64;
        ((c*c+s*s).to_bits(), *cost, std::cmp::Reverse(*speed), number(offer, "slot"))
    }).map(|(offer, _, _)| json!({"inputRate":offer["inputRate"], "cachedRate":offer["cachedRate"], "outputRate":offer["outputRate"]}));
    let mut entry = serde_json::Map::new();
    for key in [
        "id",
        "name",
        "address",
        "contextTokens",
        "outputTokens",
        "variants",
        "settings",
        "effort",
        "capabilities",
    ] {
        if let Some(value) = model.get(key) {
            entry.insert(key.into(), value.clone());
        }
    }
    entry.insert("inventory".into(), json!({"context":context,"output":output,"modalities":{"input":if image {vec!["text","image"]} else {vec!["text"]},"output":["text"]},"price":price}));
    entry.insert("offers".into(), json!([]));
    Some(Value::Object(entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_size_is_independent_of_provider_count_and_books_bind_one_model() {
        let offer = json!({"active":true,"available":true,"checkpoint":"one","checkpointOrder":1,"contextTokens":4096,"outputTokens":1024,"modalities":{"input":["text","image","audio"],"output":["text","video"]},"maxImageTokens":512,"inputRate":3,"cachedRate":1,"outputRate":6,"provider":{"endpoint":"https://example.test/infer"}});
        let mut catalog = json!({"ready":true,"slot":20,"programId":"p","market":"m","providerList":"l","providerListFeeBps":30,"aliases":{},"providers":[{"id":"irrelevant"}],"models":[{"id":"a","offers":[offer.clone()]},{"id":"b","offers":[offer.clone()]}]});
        let expected = project(&catalog, None);
        catalog["models"][0]["offers"] = json!(vec![offer; 2000]);
        assert_eq!(project(&catalog, None), expected);
        let inventory = &expected["models"][0]["inventory"];
        assert_eq!(
            inventory["modalities"],
            json!({"input":["text","image"],"output":["text"]})
        );
        assert_eq!(inventory["price"]["inputRate"], 3);
        let book = project(&catalog, Some("b"));
        assert!(book.get("providers").is_none());
        assert_eq!(book["models"], json!([catalog["models"][1]]));
        assert_eq!(book["providerListFeeBps"], 30);
        assert_eq!(project(&catalog, Some("missing"))["models"], json!([]));
    }
}
