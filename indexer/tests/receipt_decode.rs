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
