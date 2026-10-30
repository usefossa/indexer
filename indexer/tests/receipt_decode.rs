use fossa_indexer::receipts::decode;
use serde_json::{Value, json};
use solana_pubkey::Pubkey;

fn fixture() -> (Value, String, String) {
    let keys: Vec<_> = (1..=13)
        .map(|i| Pubkey::new_from_array([i; 32]).to_string())
        .collect();
    let mut data = vec![0; 157];
    data[0] = 7;
    for (at, value) in [(1, 10u32), (5, 20), (9, 30)] {
        data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    data[13..21].copy_from_slice(&7u64.to_le_bytes());
    data[21..29].copy_from_slice(&100i64.to_le_bytes());
    data[29..37].copy_from_slice(&2u64.to_le_bytes());
    data[37..69].copy_from_slice(&[99; 32]);
    data[69..101].copy_from_slice(&[4; 32]);
    data[101..133].copy_from_slice(&[5; 32]);
    data[133..141].copy_from_slice(&7u64.to_le_bytes());
    data[141..149].copy_from_slice(&9_007_199_254_740_993u64.to_le_bytes());
    data[149..157].copy_from_slice(&4000i64.to_le_bytes());
    (
        json!({"slot":55,"blockTime":123,"meta":{"err":null,"loadedAddresses":{"writable":keys[6..12],"readonly":[]}},"transaction":{"message":{"accountKeys":([keys[..6].to_vec(),vec![keys[12].clone()]].concat()),"instructions":[{"programIdIndex":6,"accounts":[0,1,2,3,4,5,7,8,9,10,11,12],"data":bs58::encode(data).into_string()}]}}}),
        keys[12].clone(),
        keys[0].clone(),
    )
}
#[test]
fn decodes_successful_top_level_claim_without_logs_and_with_lookup_addresses() {
    let (mut tx, program, market) = fixture();
    let receipts = decode(&tx, &program, &market, "signature");
    assert_eq!(receipts.len(), 1);
    let r = &receipts[0];
    assert_eq!(r.charge, "9007199254740993");
    assert_eq!(r.tokens, 60);
    assert_eq!(r.generation, "7");
    assert_eq!(r.time, 123);
    assert_eq!(r.index, 2);
    tx["meta"]["err"] = json!({"InstructionError":[0,"Custom"]});
    assert!(decode(&tx, &program, &market, "signature").is_empty());
}
#[test]
fn rejects_wrong_market_truncated_claim_mismatched_booking_and_cpi_only() {
    let (tx, program, market) = fixture();
    assert!(decode(&tx, &program, "wrong", "signature").is_empty());
    for at in [0, 70, 110, 134] {
        let mut tx = tx.clone();
        let raw = tx["transaction"]["message"]["instructions"][0]["data"]
            .as_str()
            .unwrap();
        let mut bytes = bs58::decode(raw).into_vec().unwrap();
        bytes[at] ^= 1;
        tx["transaction"]["message"]["instructions"][0]["data"] =
            bs58::encode(bytes).into_string().into();
        assert!(decode(&tx, &program, &market, "signature").is_empty());
    }
    let mut tx = tx;
    tx["meta"]["innerInstructions"] =
        json!([{"index":0,"instructions":tx["transaction"]["message"]["instructions"]}]);
    tx["transaction"]["message"]["instructions"] = json!([]);
    assert!(decode(&tx, &program, &market, "signature").is_empty());
}

#[test]
fn permissionless_claims_preserve_zero_charges_and_reusable_identity_without_buckets() {
    let (mut tx, program, market) = fixture();
    let instruction = &mut tx["transaction"]["message"]["instructions"][0];
    let old = bs58::decode(instruction["data"].as_str().unwrap())
        .into_vec()
        .unwrap();
    let mut data = [&old[..21], &old[37..]].concat();
    data[125..133].fill(0);
    instruction["accounts"].as_array_mut().unwrap().pop();
    instruction["data"] = bs58::encode(&data).into_string().into();
    let receipts = decode(&tx, &program, &market, "new-signature");
    assert_eq!(receipts.len(), 1);
    let r = &receipts[0];
    assert_eq!(r.version, 2);
    assert_eq!(r.charge, "0");
    assert_eq!(r.epoch, 0);
    assert_eq!(r.index, 7);
    assert_eq!(r.bucket, r.booking);
    assert_eq!(r.deadline, 4000);
    assert_eq!(r.tokens, 60);
    for at in [0, 13, 54, 86, 118] {
        let mut tx = tx.clone();
        let mut bad = data.clone();
        bad[at] ^= 1;
        tx["transaction"]["message"]["instructions"][0]["data"] =
            bs58::encode(bad).into_string().into();
        assert!(decode(&tx, &program, &market, "new-signature").is_empty());
    }
    let old = fixture();
    let old = decode(&old.0, &old.1, &old.2, "old-signature")
        .pop()
        .unwrap();
    assert_eq!(old.version, 1);
    let mut archived = serde_json::to_value(&old).unwrap();
    archived.as_object_mut().unwrap().remove("version");
    assert_eq!(
        serde_json::from_value::<fossa_indexer::receipts::Receipt>(archived).unwrap(),
        old
    );
}

#[test]
fn booking_list_attribution_decodes_only_successful_exact_top_level_reservations() {
    use fossa_indexer::receipts::decode_bookings;
    let (mut tx, program, market) = fixture();
    let mut data = vec![0; 116];
    data[0] = 6;
    data[108..116].copy_from_slice(&7u64.to_le_bytes());
    let instruction = &mut tx["transaction"]["message"]["instructions"][0];
    instruction["accounts"] = json!([0, 1, 2, 3, 4, 5, 7, 8, 9]);
    instruction["data"] = bs58::encode(&data).into_string().into();
    let rows = decode_bookings(&tx, &program, &market);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].generation, "7");
    assert_eq!(rows[0].list, Pubkey::new_from_array([9; 32]).to_string());
    assert_eq!(rows[0].booking, Pubkey::new_from_array([7; 32]).to_string());
    assert!(decode_bookings(&tx, &program, "foreign").is_empty());
    for generation in [1 << 56, u64::MAX] {
        let mut tx = tx.clone();
        let mut invalid = data.clone();
        invalid[108..116].copy_from_slice(&generation.to_le_bytes());
        tx["transaction"]["message"]["instructions"][0]["data"] =
            bs58::encode(invalid).into_string().into();
        assert!(decode_bookings(&tx, &program, &market).is_empty());
    }
    for length in [0, 107, 109, 115, 117] {
        let mut tx = tx.clone();
        let mut invalid = data.clone();
        invalid.resize(length, 0);
        tx["transaction"]["message"]["instructions"][0]["data"] =
            bs58::encode(invalid).into_string().into();
        assert!(decode_bookings(&tx, &program, &market).is_empty());
    }
    let mut legacy = tx.clone();
    legacy["transaction"]["message"]["instructions"][0]["data"] =
        bs58::encode(&data[..108]).into_string().into();
    assert_eq!(
        decode_bookings(&legacy, &program, &market)[0].generation,
        "0"
    );
    let mut failed = tx.clone();
    failed["meta"]["err"] = json!({"InstructionError":[0,"Custom"]});
    assert!(decode_bookings(&failed, &program, &market).is_empty());
    let mut cpi = tx.clone();
    cpi["meta"]["innerInstructions"] =
        json!([{"index":0,"instructions":cpi["transaction"]["message"]["instructions"]}]);
    cpi["transaction"]["message"]["instructions"] = json!([]);
    assert!(decode_bookings(&cpi, &program, &market).is_empty());
    tx["transaction"]["message"]["instructions"][0]["accounts"][8] = 999.into();
    assert!(decode_bookings(&tx, &program, &market).is_empty());
}

#[test]
fn native_receipts_decode_full_generation_with_pinned_fee_accounts_and_borsh_option() {
    use sha2::{Digest, Sha256};
    let (mut tx, program, market) = fixture();
    let instruction = &mut tx["transaction"]["message"]["instructions"][0];
    let old = bs58::decode(instruction["data"].as_str().unwrap())
        .into_vec()
        .unwrap();
    let mut data = [
        &Sha256::digest(b"global:claim_booking")[..8],
        &old[1..13],
        &[1],
        &old[37..],
    ]
    .concat();
    data[117..125].copy_from_slice(&u64::MAX.to_le_bytes());
    instruction["accounts"] = json!([0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 8, 12]);
    instruction["data"] = bs58::encode(&data).into_string().into();
    let receipt = decode(&tx, &program, &market, "native").pop().unwrap();
    assert_eq!(receipt.generation, u64::MAX.to_string());
    assert_eq!(receipt.charge, "9007199254740993");
    assert_eq!(receipt.tokens, 60);
    assert_eq!(
        receipt.lane().unwrap(),
        Pubkey::new_from_array([4; 32]).to_string()
    );
    for offset in [0, 20, 53, 85] {
        let mut invalid = tx.clone();
        let mut bytes = data.clone();
        bytes[offset] ^= 1;
        invalid["transaction"]["message"]["instructions"][0]["data"] =
            bs58::encode(bytes).into_string().into();
        assert!(decode(&invalid, &program, &market, "invalid").is_empty());
    }
}

#[test]
fn native_booking_attribution_reads_full_u64_after_option_tag() {
    use fossa_indexer::receipts::decode_bookings;
    use sha2::{Digest, Sha256};
    let (mut tx, program, market) = fixture();
    let mut data = vec![0; 124];
    data[..8].copy_from_slice(&Sha256::digest(b"global:reserve_booking")[..8]);
    data[115] = 1;
    data[116..124].copy_from_slice(&u64::MAX.to_le_bytes());
    let instruction = &mut tx["transaction"]["message"]["instructions"][0];
    instruction["accounts"] = json!([0, 1, 2, 3, 4, 5, 7, 8, 9, 6]);
    instruction["data"] = bs58::encode(&data).into_string().into();
    assert_eq!(
        decode_bookings(&tx, &program, &market)[0].generation,
        u64::MAX.to_string()
    );
    data[116..124].fill(0);
    tx["transaction"]["message"]["instructions"][0]["data"] =
        bs58::encode(&data).into_string().into();
    assert!(decode_bookings(&tx, &program, &market).is_empty());
    data[115] = 0;
    tx["transaction"]["message"]["instructions"][0]["data"] =
        bs58::encode(data).into_string().into();
    assert!(decode_bookings(&tx, &program, &market).is_empty());
}
