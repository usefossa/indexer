use super::*;

pub fn market_config(data: &[u8]) -> Option<Value> {
    (data.starts_with(&MARKET_DISCRIMINATOR) && kind(data) == Some(Kind::Market)).then(|| {
        serde_json::json!({
            "council": address(data, 8), "bank": address(data, 72),
            "creationMint": optional_address(data, 104),
            "creationTokenProgram": optional_address(data, 136),
            "creationNetPrice": u64_at(data, 168).to_string(),
            "creationEnabled": data[176] == 1,
            "listRoutingEnabled": data[177] >= 1,
            "providerLimitsEnabled": data[177] == 2,
            "dexProgram": optional_address(data, 184), "dexConfig": optional_address(data, 216)
        })
    })
}

pub fn provider_limit_values<'a>(
    records: impl Iterator<Item = (&'a str, &'a [u8])> + Clone,
    config: &Config,
) -> std::collections::BTreeMap<String, Value> {
    let pages: std::collections::BTreeMap<_, _> = records
        .clone()
        .filter(|(address, data)| {
            data.starts_with(&PROVIDER_LIMIT_PAGE_DISCRIMINATOR)
                && validate(address, data, config) == Some(Kind::ProviderLimitPage)
        })
        .map(|(address, data)| (data[41], (address, data)))
        .collect();
    records.filter_map(|(identity, data)| {
        if !data.starts_with(&PROVIDER_LIMIT_INDEX_DISCRIMINATOR)
            || validate(identity, data, config) != Some(Kind::ProviderLimitIndex) {
            return None;
        }
        let id = u32_at(data, 72);
        let wallet = address(data, 40);
        let (page, data) = pages.get(&((id & 31) as u8))?;
        let index = (id >> 5) as usize;
        if index >= u32_at(data, 44) as usize { return None; }
        let row = &data[48 + index * 40..48 + (index + 1) * 40];
        Some((wallet, serde_json::json!({
            "providerLimitId":id,"providerLimitPage":page,
            "providerLimits":{"global":{"concurrency":u32_at(row,0),"quota":u32_at(row,4),"quotaWindowSeconds":u32_at(row,8)},
                "wallet":{"concurrency":u32_at(row,28),"quota":u32_at(row,32),"quotaWindowSeconds":u32_at(row,36)}},
            "activeBookings":u32_at(row,24),"quotaUsed":u32_at(row,20),"quotaWindowEnd":i64_at(row,12),
            "bookingLimits":{"walletWindowSeconds":u32_at(row,36),"walletBookingsPerWindow":u32_at(row,32),"walletBookingCapacity":u32_at(row,28)}
        })))
    }).collect()
}

pub fn provider_policy_value(data: &[u8]) -> Option<Value> {
    (kind(data) == Some(Kind::ProviderPolicy)).then(||serde_json::json!({
        "walletWindowSeconds":u32_at(data,72),"walletBookingsPerWindow":u32_at(data,76),"walletBookingCapacity":u32_at(data,80),
        "policyEpoch":u64_at(data,84).to_string(),"updating":data[96]==1
    }))
}

pub fn booking_limits<'a>(
    records: impl Iterator<Item = &'a [u8]> + Clone,
) -> std::collections::BTreeMap<String, Value> {
    let mut values = records
        .clone()
        .filter(|data| kind(data) == Some(Kind::BookingPolicies))
        .flat_map(booking_policy_values)
        .collect::<std::collections::BTreeMap<_, _>>();
    for data in records {
        if let Some(value) = provider_policy_value(data) {
            values.insert(address(data, 40), value);
        }
    }
    values
}

pub fn routing_policy_values(data: &[u8]) -> Option<std::collections::BTreeMap<String, Value>> {
    if kind(data) != Some(Kind::ListRouting) {
        return None;
    }
    Some(data[84..84+u32_at(data,80) as usize*56].chunks_exact(56).filter(|row|row[52]==1).map(|row|(
        address(row,0),serde_json::json!({"walletWindowSeconds":u32_at(row,32),"walletBookingsPerWindow":u32_at(row,36),
            "walletBookingCapacity":u32_at(row,40),"policyEpoch":u64_at(row,44).to_string()})
    )).collect())
}

pub fn optional_address(data: &[u8], offset: usize) -> Option<String> {
    key(data, offset)
        .filter(|key| *key != Pubkey::default())
        .map(|key| key.to_string())
}
pub fn booking_policy_values(data: &[u8]) -> std::collections::BTreeMap<String, Value> {
    if kind(data) != Some(Kind::BookingPolicies) {
        return Default::default();
    }
    (0..u32_at(data, 48) as usize).filter_map(|index| {
        let at = 52 + index * 44;
        let seconds = u32_at(data, at + 32);
        (key(data, at)? != Pubkey::default() && (1..=86_400).contains(&seconds)).then(|| (
            address(data, at), serde_json::json!({"providerPolicy":index,"walletWindowSeconds":seconds,
                "walletBookingsPerWindow":u32_at(data,at+36),"walletBookingCapacity":u32_at(data,at+40)})
        ))
    }).collect()
}
pub fn model_group(data: &[u8], address: &str) -> Value {
    serde_json::json!({"address": address, "groupId": self::address(data, 40),
        "backingVault": self::address(data, 72), "tokenMint": self::address(data, 104),
        "routeToReserve": data[136] == 1})
}
pub fn booking_fees(data: &[u8]) -> Option<Value> {
    ((data.starts_with(&BOOKING_DISCRIMINATOR) || data.starts_with(&LEGACY_BOOKING_DISCRIMINATOR))
        && kind(data) == Some(Kind::Booking))
    .then(|| {
        serde_json::json!({"creator": optional_address(data, 368),
            "protocolDestination": optional_address(data, 400),
            "reserveAuthority": optional_address(data, 432), "feeBps": u16_at(data, 464),
            "protocolShareBps": u16_at(data, 466), "reserveDestination": data[468] == 1})
    })
}
pub fn metadata_uri(data: &[u8], hash: usize, len: usize, at: usize) -> Option<(&[u8], &str)> {
    let length = u16_at(data, len) as usize;
    if !(1..=256).contains(&length) {
        return None;
    }
    let uri = std::str::from_utf8(data.get(at..at + length)?).ok()?;
    if uri
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || data.get(at + length..)?.iter().any(|v| *v != 0)
    {
        return None;
    }
    Some((data.get(hash..hash + 32)?, uri))
}
