use super::*;

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
        d if d == MARKET_DISCRIMINATOR && data.len() == 248 && data[176] <= 1 && data[177] <= 2 => {
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
            && data[113] <= 1
            && data[114..116] == [0; 2]
            && data[120..120 + u32_at(data, 116) as usize * 140]
                .chunks_exact(140)
                .all(|row| (1..=2).contains(&row[114]) && (data[113] == 0 || row[114] == 2)) =>
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
        d if d == PROVIDER_LIMIT_PAGE_DISCRIMINATOR
            && (48..=MAX_ACCOUNT_BYTES).contains(&data.len())
            && (data.len() - 48).is_multiple_of(40)
            && u32_at(data, 44) as usize <= (data.len() - 48) / 40
            && data[41] < 32
            && data[42..44] == [0; 2]
            && data[48..48 + u32_at(data, 44) as usize * 40]
                .chunks_exact(40)
                .all(|row| {
                    u32_at(row, 0) != 0
                        && (1..=86_400).contains(&u32_at(row, 8))
                        && (1..=86_400).contains(&u32_at(row, 36))
                }) =>
        {
            Kind::ProviderLimitPage
        }
        d if d == PROVIDER_LIMIT_INDEX_DISCRIMINATOR
            && data.len() == 80
            && data[40..72] != [0; 32]
            && data[77..80] == [0; 3] =>
        {
            Kind::ProviderLimitIndex
        }
        d if d == MODELMETADATA_DISCRIMINATOR && data.len() == 396 => Kind::Metadata,
        d if d == LISTMETADATA_DISCRIMINATOR && data.len() == 396 => Kind::ListMetadata,
        d if d == USER_DISCRIMINATOR && data.len() == 128 => Kind::User,
        d if (d == BOOKING_DISCRIMINATOR && data.len() == BOOKING_LEN
            || d == LEGACY_BOOKING_DISCRIMINATOR && data.len() == 472)
            && data[148] <= 5
            && data[356] <= if d == BOOKING_DISCRIMINATOR { 3 } else { 2 } =>
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
        Kind::ProviderLimitPage => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[
                        b"provider_limit_page",
                        config.market.as_ref(),
                        &data[41..42],
                    ],
                    data[40],
                    &config.program,
                )
        }
        Kind::ProviderLimitIndex => {
            key(data, 8)? == config.market
                && pda(
                    &address,
                    &[b"provider_limits", config.market.as_ref(), &data[40..72]],
                    data[76],
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

fn sorted_slab(data: &[u8], offset: usize, stride: usize, limit_at: Option<usize>) -> bool {
    if data.len() < offset
        || data.len() > MAX_ACCOUNT_BYTES
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
