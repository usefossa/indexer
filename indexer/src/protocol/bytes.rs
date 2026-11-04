use super::*;

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

pub fn bytes(value: &Value) -> Option<Vec<u8>> {
    if value["data"][1] != "base64" || value["executable"] == true {
        return None;
    }
    let encoded = value["data"][0].as_str()?;
    if encoded.len() > MAX_ACCOUNT_BYTES.div_ceil(3) * 4 {
        return None;
    }
    STANDARD
        .decode(encoded)
        .ok()
        .filter(|data| data.len() <= MAX_ACCOUNT_BYTES)
}
