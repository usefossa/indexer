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
pub const BOOKING_LEN: usize = 368;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Market,
    Model,
    Provider,
    Metadata,
    Terms,
    Capacity,
    User,
    Booking,
    Unbond,
    Receipt,
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
pub fn kind(data: &[u8]) -> Option<Kind> {
    Some(match data.get(..8)? {
        b"FOSSAV20" if data.len() == 72 => Kind::Market,
        b"MODELV20" if data.len() == 144 => Kind::Model,
        b"PROV0V20" if data.len() == 364 => Kind::Provider,
        b"MMETAV20" if data.len() == 396 => Kind::Metadata,
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
        b"BOOK0V20" if data.len() == BOOKING_LEN && data[148] <= 5 => Kind::Booking,
        b"UNBNDV20" if data.len() == 160 && data[88] <= 1 => Kind::Unbond,
        b"RECEIPT1" if data.len() == 736 => Kind::Receipt,
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
        Kind::Provider => {
            key(data, 8)? == config.market
                && data[106] <= 1
                && pda(
                    &address,
                    &[b"provider", config.market.as_ref(), &data[40..72]],
                    data[107],
                    &config.program,
                )
                && metadata_uri(data, 72, 104, 108).is_some()
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
        Kind::Terms | Kind::Capacity => true,
        Kind::User => {
            key(data, 40)? == config.market
                && key(data, 80)? == vault(&address, config)
                && data[112]
                    == Pubkey::find_program_address(&[b"vault", address.as_ref()], &config.program)
                        .1
        }
        Kind::Booking => key(data, 72)? == config.market,
        Kind::Unbond | Kind::Receipt => key(data, 8)? == config.market,
    };
    valid.then_some(kind)
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
