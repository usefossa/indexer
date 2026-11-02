use crate::{
    config::Config,
    error::{Error, Result},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use solana_pubkey::Pubkey;
use std::str::FromStr;

pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ATA: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const BOOKING_LEN: usize = 520;
pub const TERMS_LEN: usize = 132;
pub const LEGACY_TERMS_DISCRIMINATOR: [u8; 8] = [223, 24, 40, 223, 249, 219, 14, 97];
pub const LEGACY_BOOKING_DISCRIMINATOR: [u8; 8] = [147, 50, 61, 138, 208, 21, 254, 156];
pub const MARKET_DISCRIMINATOR: [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
pub const MODEL_DISCRIMINATOR: [u8; 8] = [152, 221, 247, 122, 185, 125, 223, 151];
pub const MODEL_GROUP_DISCRIMINATOR: [u8; 8] = [237, 175, 79, 136, 212, 228, 240, 177];
pub const PROVIDER_DISCRIMINATOR: [u8; 8] = [164, 180, 71, 17, 75, 216, 80, 195];
pub const MODELMETADATA_DISCRIMINATOR: [u8; 8] = [243, 161, 2, 132, 123, 88, 51, 61];
pub const LISTMETADATA_DISCRIMINATOR: [u8; 8] = [245, 77, 238, 20, 114, 17, 94, 226];
pub const TERMS_DISCRIMINATOR: [u8; 8] = [136, 206, 216, 233, 146, 248, 21, 65];
pub const BOOKING_TERMS_DISCRIMINATOR: [u8; 8] = [7, 171, 147, 70, 168, 59, 177, 8];
pub const BOOKING_POLICIES_DISCRIMINATOR: [u8; 8] = [63, 176, 147, 212, 151, 136, 53, 228];
pub const WALLET_BOOKINGS_DISCRIMINATOR: [u8; 8] = [251, 175, 236, 250, 115, 241, 109, 202];
pub const PROVIDER_POLICY_DISCRIMINATOR: [u8; 8] = [160, 80, 153, 123, 160, 152, 122, 32];
pub const PROVIDER_CAPACITY_DISCRIMINATOR: [u8; 8] = [139, 242, 225, 128, 103, 72, 36, 45];
pub const LIST_ROUTING_DISCRIMINATOR: [u8; 8] = [144, 176, 101, 27, 253, 191, 157, 242];
pub const LIST_TERMS_DISCRIMINATOR: [u8; 8] = [174, 240, 243, 180, 203, 181, 0, 107];
pub const LIST_CAPACITY_DISCRIMINATOR: [u8; 8] = [73, 4, 213, 31, 149, 94, 219, 141];
pub const PROVIDER_WALLET_DISCRIMINATOR: [u8; 8] = [89, 4, 132, 220, 207, 78, 178, 52];
pub const CAPACITY_DISCRIMINATOR: [u8; 8] = [19, 216, 196, 142, 39, 29, 145, 16];
pub const USER_DISCRIMINATOR: [u8; 8] = [159, 117, 95, 227, 239, 151, 58, 236];
pub const BOOKING_DISCRIMINATOR: [u8; 8] = [151, 160, 101, 76, 250, 167, 210, 104];
pub const LIST_DISCRIMINATOR: [u8; 8] = [169, 24, 186, 110, 22, 139, 190, 82];
pub const RESERVE_BOOKING_DISCRIMINATOR: [u8; 8] = [170, 28, 99, 26, 95, 11, 225, 109];
pub const RESERVE_LIST_BOOKING_DISCRIMINATOR: [u8; 8] = [170, 22, 43, 144, 39, 255, 146, 208];
pub const CLAIM_LIST_BOOKING_DISCRIMINATOR: [u8; 8] = [50, 202, 193, 96, 27, 61, 152, 239];
pub const CLAIM_BOOKING_DISCRIMINATOR: [u8; 8] = [143, 218, 32, 94, 7, 208, 186, 252];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Market,
    Model,
    ModelGroup,
    Provider,
    Metadata,
    Terms,
    Capacity,
    User,
    Booking,
    ProviderList,
    ListMetadata,
    BookingPolicies,
    WalletBookings,
    ProviderPolicy,
    ProviderCapacity,
    ListRouting,
    ListTerms,
    ListCapacity,
    ProviderWallet,
}
pub fn key(data: &[u8], offset: usize) -> Option<Pubkey> {
    Some(Pubkey::new_from_array(
        data.get(offset..offset + 32)?.try_into().ok()?,
    ))
}
pub fn address(data: &[u8], offset: usize) -> String {
    key(data, offset).map(|v| v.to_string()).unwrap_or_default()
}
pub fn u16_at(d: &[u8], at: usize) -> u16 {
    d.get(at..at + 2)
        .and_then(|d| d.try_into().ok())
        .map(u16::from_le_bytes)
        .unwrap_or(0)
}
pub fn u32_at(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 4)
        .and_then(|d| d.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}
pub fn u64_at(d: &[u8], at: usize) -> u64 {
    d.get(at..at + 8)
        .and_then(|d| d.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}
pub fn i64_at(d: &[u8], at: usize) -> i64 {
    u64_at(d, at) as i64
}
pub fn pubkey(value: &str) -> Result<Pubkey> {
    Pubkey::from_str(value).map_err(|_| Error::bad("Invalid public key"))
}
pub fn associated(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            owner.as_ref(),
            &pubkey(TOKEN).expect("token id").to_bytes(),
            mint.as_ref(),
        ],
        &pubkey(ATA).expect("ata id"),
    )
    .0
}
pub fn vault(lane: &Pubkey, config: &Config) -> Pubkey {
    associated(
        &Pubkey::find_program_address(&[b"vault", lane.as_ref()], &config.program).0,
        &config.mint,
    )
}
fn pda(address: &Pubkey, seeds: &[&[u8]], bump: u8, program: &Pubkey) -> bool {
    let (expected, canonical) = Pubkey::find_program_address(seeds, program);
    *address == expected && bump == canonical
}
fn list_tree(keys: &[u8], tree: &[u8], node: usize, at: &mut usize) -> bool {
    if node >= tree.len() / 32 {
        return true;
    }
    if !list_tree(keys, tree, node * 2 + 1, at) {
        return false;
    }
    let key = &keys[*at * 32..(*at + 1) * 32];
    if !(0..32).step_by(8).all(|offset| {
        u64_at(tree, node * 32 + offset)
            == u64::from_be_bytes(key[offset..offset + 8].try_into().unwrap())
    }) {
        return false;
    }
    *at += 1;
    list_tree(keys, tree, node * 2 + 2, at)
}
pub fn list_members(data: &[u8]) -> &[u8] {
    let start = if data.starts_with(&LIST_DISCRIMINATOR) {
        88
    } else {
        80
    };
    &data[start..start + u32_at(data, start - 4) as usize * 32]
}
pub fn list_fees(data: &[u8]) -> Option<(u16, u16)> {
    data.starts_with(&LIST_DISCRIMINATOR)
        .then(|| (u16_at(data, 76), u16_at(data, 78)))
}
pub fn kind(data: &[u8]) -> Option<Kind> {
    Some(match data.get(..8)? {
        d if d == MARKET_DISCRIMINATOR && data.len() == 248 && data[176] <= 1 && data[177] <= 1 => {
            Kind::Market
        }
        d if d == MODEL_DISCRIMINATOR && data.len() == 176 => Kind::Model,
        d if d == MODEL_GROUP_DISCRIMINATOR && data.len() == 144 && data[136] <= 1 => {
            Kind::ModelGroup
        }
        d if d == PROVIDER_DISCRIMINATOR && data.len() == 364 && data[106] <= 1 => Kind::Provider,
        d if d == BOOKING_POLICIES_DISCRIMINATOR
            && data.len() >= 52
            && (data.len() - 52).is_multiple_of(44)
            && u32_at(data, 48) as usize <= (data.len() - 52) / 44 =>
        {
            Kind::BookingPolicies
        }
        d if d == WALLET_BOOKINGS_DISCRIMINATOR
            && data.len() >= 112
            && (data.len() - 112).is_multiple_of(20)
            && u32_at(data, 108) as usize <= (data.len() - 112) / 20 =>
        {
            Kind::WalletBookings
        }
        d if d == PROVIDER_POLICY_DISCRIMINATOR
            && data.len() == 100
            && data[96] <= 1
            && data[98..] == [0; 2]
            && (1..=86_400).contains(&u32_at(data, 72))
            && u64_at(data, 84) != 0 =>
        {
            Kind::ProviderPolicy
        }
        d if d == PROVIDER_CAPACITY_DISCRIMINATOR
            && data.len() == 92
            && data[89..] == [0; 3]
            && u64::from(u32_at(data, 76)) + u64::from(u32_at(data, 80))
                <= u64::from(u32_at(data, 72)) =>
        {
            Kind::ProviderCapacity
        }
        d if d == LIST_ROUTING_DISCRIMINATOR
            && sorted_slab(data, 84, 56, None)
            && data[77..80] == [0; 3]
            && data[84..84 + u32_at(data, 80) as usize * 56]
                .chunks_exact(56)
                .all(|row| {
                    row[52] <= 1 && row[53..] == [0; 3] && (1..=86_400).contains(&u32_at(row, 32))
                }) =>
        {
            Kind::ListRouting
        }
        d if d == LIST_TERMS_DISCRIMINATOR
            && sorted_slab(data, 120, 140, Some(104))
            && data[113..116] == [0; 3] =>
        {
            Kind::ListTerms
        }
        d if d == LIST_CAPACITY_DISCRIMINATOR
            && sorted_slab(data, 116, 36, Some(104))
            && data[109..112] == [0; 3] =>
        {
            Kind::ListCapacity
        }
        d if d == PROVIDER_WALLET_DISCRIMINATOR
            && sorted_slab(data, 112, 48, None)
            && data[105..108] == [0; 3] =>
        {
            Kind::ProviderWallet
        }
        d if d == MODELMETADATA_DISCRIMINATOR && data.len() == 396 => Kind::Metadata,
        d if d == LISTMETADATA_DISCRIMINATOR && data.len() == 396 => Kind::ListMetadata,
        d if d == USER_DISCRIMINATOR && data.len() == 128 => Kind::User,
        d if (d == BOOKING_DISCRIMINATOR && data.len() == BOOKING_LEN
            || d == LEGACY_BOOKING_DISCRIMINATOR && data.len() == 472)
            && data[148] <= 5 =>
        {
            Kind::Booking
        }
        d if (d == TERMS_DISCRIMINATOR
            || d == BOOKING_TERMS_DISCRIMINATOR
            || d == LEGACY_TERMS_DISCRIMINATOR
            || d == CAPACITY_DISCRIMINATOR)
            && data.len() >= 48 =>
        {
            let terms = d != CAPACITY_DISCRIMINATOR;
            let stride = if d == BOOKING_TERMS_DISCRIMINATOR {
                136
            } else if d == TERMS_DISCRIMINATOR {
                TERMS_LEN
            } else {
                120
            };
            let limit = u32_at(data, 40) as usize;
            if !(1..=4096).contains(&limit)
                || u32_at(data, 44) as usize > limit
                || data.len() != 48 + limit * if terms { stride } else { 32 }
            {
                return None;
            }
            if terms { Kind::Terms } else { Kind::Capacity }
        }
        d if d == LIST_DISCRIMINATOR && data.len() >= 88 => {
            let limit = u32_at(data, 72) as usize;
            let count = u32_at(data, 84) as usize;
            if !(1..=4096).contains(&limit)
                || count > limit
                || data.len() != 88 + limit * 32
                || !(10..=300).contains(&u16_at(data, 76))
                || !(5000..=10000).contains(&u16_at(data, 78))
            {
                return None;
            }
            let members = list_members(data);
            if members.chunks_exact(32).any(|key| key == [0; 32])
                || (1..count)
                    .any(|i| members[(i - 1) * 32..i * 32] >= members[i * 32..(i + 1) * 32])
                || data[88 + count * 32..].iter().any(|v| *v != 0)
            {
                return None;
            }
            Kind::ProviderList
        }
        b"FOSSAV20" if data.len() == 72 => Kind::Market,
        b"MODELV20" if data.len() == 144 => Kind::Model,
        b"PROV0V20" if data.len() == 364 => Kind::Provider,
        b"MMETAV20" if data.len() == 396 => Kind::Metadata,
        b"LMETAV30" if data.len() == 396 => Kind::ListMetadata,
        b"TERMSV20" | b"CAPACV20" if data.len() >= 48 => {
            let terms = data.starts_with(b"TERMS");
            let limit = u32_at(data, 40) as usize;
            if !(1..=4096).contains(&limit)
                || u32_at(data, 44) as usize > limit
                || data.len() != 48 + limit * if terms { 120 } else { 32 }
            {
                return None;
            }
            if terms { Kind::Terms } else { Kind::Capacity }
        }
        b"USER0V20" if data.len() == 120 => Kind::User,
        b"BOOK0V20" if data.len() == 368 && data[148] <= 5 => Kind::Booking,
        b"LIST0V30" if data.len() >= 80 => {
            let limit = u32_at(data, 72) as usize;
            let count = u32_at(data, 76) as usize;
            if !(1..=4096).contains(&limit) || count > limit || data.len() != 80 + limit * 64 {
                return None;
            }
            let members = &data[80..80 + count * 32];
            let tree = &data[80 + limit * 32..];
            if members.chunks_exact(32).any(|key| key == [0; 32])
                || (1..count)
                    .any(|i| members[(i - 1) * 32..i * 32] >= members[i * 32..(i + 1) * 32])
                || data[80 + count * 32..80 + limit * 32]
                    .iter()
                    .any(|v| *v != 0)
                || tree[count * 32..].iter().any(|v| *v != 0)
                || !list_tree(members, &tree[..count * 32], 0, &mut 0)
            {
                return None;
            }
            Kind::ProviderList
        }
        _ => return None,
    })
}
pub fn validate(address: &str, data: &[u8], config: &Config) -> Option<Kind> {
    let address = Pubkey::from_str(address).ok()?;
    let kind = kind(data)?;
    let valid = match kind {
        Kind::Market => address == config.market && key(data, 40)? == config.mint,
        Kind::Model => {
            key(data, 8)? == config.market
                && (1..=4096).contains(&u32_at(data, 136))
                && key(data, 72)? != key(data, 104)?
                && pda(
                    &address,
                    &[b"model", config.market.as_ref(), &data[40..72]],
                    data[140],
                    &config.program,
                )
        }
        Kind::ModelGroup => {
            key(data, 8)? == config.market
                && key(data, 72)? != Pubkey::default()
                && data[138..].iter().all(|byte| *byte == 0)
                && key(data, 104)?
                    == Pubkey::find_program_address(
                        &[b"reserve_mint", address.as_ref()],
                        &config.program,
                    )
                    .0
                && pda(
                    &address,
                    &[b"model_group", config.market.as_ref(), &data[40..72]],
                    data[137],
                    &config.program,
                )
        }
        Kind::Provider => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[b"provider", config.market.as_ref(), &data[40..72]],
                    data[107],
                    &config.program,
                )
                && metadata_uri(data, 72, 104, 108).is_some()
        }
        Kind::BookingPolicies => {
            key(data, 8)? == config.market
                && data[45..48] == [0; 3]
                && pda(
                    &address,
                    &[b"booking_policies", config.market.as_ref()],
                    data[44],
                    &config.program,
                )
        }
        Kind::WalletBookings => {
            key(data, 8)? == config.market
                && data[105..108] == [0; 3]
                && pda(
                    &address,
                    &[b"wallet_bookings", config.market.as_ref(), &data[40..72]],
                    data[104],
                    &config.program,
                )
        }
        Kind::ProviderPolicy => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[b"provider_policy", config.market.as_ref(), &data[40..72]],
                    data[97],
                    &config.program,
                )
        }
        Kind::ProviderCapacity => pda(
            &address,
            &[b"provider_capacity", &data[8..40], &data[40..72]],
            data[88],
            &config.program,
        ),
        Kind::ListRouting => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[b"list_routing", &data[40..72]],
                    data[76],
                    &config.program,
                )
        }
        Kind::ListTerms => pda(
            &address,
            &[b"list_terms", &data[40..72], &data[8..40]],
            data[112],
            &config.program,
        ),
        Kind::ListCapacity => pda(
            &address,
            &[b"list_capacity", &data[40..72], &data[8..40]],
            data[108],
            &config.program,
        ),
        Kind::ProviderWallet => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[b"provider_wallet", config.market.as_ref(), &data[40..72]],
                    data[104],
                    &config.program,
                )
        }
        Kind::Metadata => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[b"model_metadata", &data[40..72]],
                    data[138],
                    &config.program,
                )
                && metadata_uri(data, 104, 136, 140).is_some()
        }
        Kind::ListMetadata => {
            key(data, 8)? == config.market
                && data[139] == 0
                && pda(
                    &address,
                    &[b"list_metadata", &data[40..72]],
                    data[138],
                    &config.program,
                )
                && metadata_uri(data, 104, 136, 140).is_some()
        }
        Kind::Terms | Kind::Capacity => true,
        Kind::User => {
            key(data, 40)? == config.market
                && key(data, 80)? == vault(&address, config)
                && data[if data.len() == 128 { 120 } else { 112 }]
                    == Pubkey::find_program_address(&[b"vault", address.as_ref()], &config.program)
                        .1
        }
        Kind::Booking => key(data, 72)? == config.market,
        Kind::ProviderList => key(data, 8)? == config.market,
    };
    valid.then_some(kind)
}
pub fn market_config(data: &[u8]) -> Option<Value> {
    (data.starts_with(&MARKET_DISCRIMINATOR) && kind(data) == Some(Kind::Market)).then(|| {
        serde_json::json!({
            "council": address(data, 8), "bank": address(data, 72),
            "creationMint": optional_address(data, 104),
            "creationTokenProgram": optional_address(data, 136),
            "creationNetPrice": u64_at(data, 168).to_string(),
            "creationEnabled": data[176] == 1,
            "listRoutingEnabled": data[177] == 1,
            "dexProgram": optional_address(data, 184), "dexConfig": optional_address(data, 216)
        })
    })
}
fn sorted_slab(data: &[u8], offset: usize, stride: usize, limit_at: Option<usize>) -> bool {
    if data.len() < offset
        || data.len() > 10 * 1024 * 1024
        || !(data.len() - offset).is_multiple_of(stride)
    {
        return false;
    }
    let capacity = (data.len() - offset) / stride;
    let count = u32_at(data, offset - 4) as usize;
    if count > capacity
        || limit_at.is_some_and(|at| {
            capacity == 0 || capacity > 4096 || u32_at(data, at) as usize != capacity
        })
    {
        return false;
    }
    let rows = &data[offset..offset + count * stride];
    rows.chunks_exact(stride).all(|row| row[..32] != [0; 32])
        && rows
            .chunks_exact(stride)
            .zip(rows.chunks_exact(stride).skip(1))
            .all(|(a, b)| a[..32] < b[..32])
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

pub fn list_books(program: &Pubkey, list: &Pubkey, model: &Pubkey) -> (String, String) {
    (
        Pubkey::find_program_address(&[b"list_terms", list.as_ref(), model.as_ref()], program)
            .0
            .to_string(),
        Pubkey::find_program_address(&[b"list_capacity", list.as_ref(), model.as_ref()], program)
            .0
            .to_string(),
    )
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
pub fn bytes(value: &Value) -> Option<Vec<u8>> {
    if value["data"][1] != "base64" || value["executable"] == true {
        return None;
    }
    let encoded = value["data"][0].as_str()?;
    if encoded.len() > 660_000 {
        return None;
    }
    STANDARD.decode(encoded).ok()
}
