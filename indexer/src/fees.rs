use crate::{now, rpc::Rpc};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};

const SAMPLE_AGE: u64 = 30;
const MAX_SAMPLES: usize = 256;
const MIN_SAMPLES: usize = 8;
const FALLBACK_AGE: u64 = 120;
const MAX_WIRE_PRICE: u64 = 9_007_199_254_740_991;

#[derive(Clone)]
pub struct Fees {
    pub updates: watch::Sender<u64>,
    samples: Arc<Mutex<Samples>>,
}

impl Default for Fees {
    fn default() -> Self {
        Self {
            updates: watch::channel(0).0,
            samples: Arc::default(),
        }
    }
}

#[derive(Default)]
struct Samples {
    values: VecDeque<Sample>,
    published_at: u64,
}

struct Sample {
    at: u64,
    slot: u64,
    signature: [u8; 64],
    price: u64,
}

fn percentile(mut prices: Vec<u64>) -> Option<u64> {
    let index = prices.len().checked_sub(1)? * 3 / 4;
    Some(*prices.select_nth_unstable(index).1)
}

impl Samples {
    fn insert(&mut self, sample: Sample, at: u64) -> bool {
        if sample.slot == 0
            || sample.at == 0
            || sample.at > at.saturating_add(5)
            || at.saturating_sub(sample.at) >= SAMPLE_AGE
            || self
                .values
                .iter()
                .any(|old| old.signature == sample.signature)
        {
            return false;
        }
        self.values.push_back(sample);
        if self.values.len() > MAX_SAMPLES {
            self.values.pop_front();
        }
        true
    }

    fn estimate(&mut self, at: u64) -> Option<u64> {
        self.values
            .retain(|sample| at.saturating_sub(sample.at) < SAMPLE_AGE);
        let first = self.values.front()?;
        if self.values.len() < MIN_SAMPLES
            || self.values.iter().all(|sample| sample.slot == first.slot)
        {
            return None;
        }
        percentile(self.values.iter().map(|sample| sample.price).collect())
    }
}

impl Fees {
    pub fn snapshot(&self) -> Value {
        json!({
            "baselineMicroLamports": (*self.updates.borrow()).min(MAX_WIRE_PRICE),
            "baselineFloorLamports": 1_000,
            "baselineCapLamports": 10_000,
        })
    }

    pub async fn observe(
        &self,
        slot: u64,
        signature: [u8; 64],
        created_at: u64,
        micro_lamports: u64,
    ) {
        self.observe_at(
            Sample {
                at: created_at,
                slot,
                signature,
                price: micro_lamports,
            },
            now(),
        )
        .await;
    }

    async fn observe_at(&self, sample: Sample, at: u64) {
        let mut samples = self.samples.lock().await;
        if samples.insert(sample, at)
            && samples.published_at < at
            && let Some(price) = samples.estimate(at)
        {
            samples.published_at = at;
            self.publish(price);
        }
    }

    fn publish(&self, price: u64) {
        self.updates.send_if_modified(|previous| {
            if *previous == price {
                return false;
            }
            *previous = price;
            true
        });
    }

    async fn publish_current(&self, at: u64, fallback: Option<(u64, u64)>) -> bool {
        let mut samples = self.samples.lock().await;
        let streamed = samples.estimate(at);
        self.publish(
            streamed
                .or_else(|| {
                    fallback
                        .filter(|(received, _)| at.saturating_sub(*received) < FALLBACK_AGE)
                        .map(|(_, price)| price)
                })
                .unwrap_or(0),
        );
        streamed.is_none()
    }
}

fn fallback_price(value: &Value) -> Option<u64> {
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 256)?;
    let mut slots = std::collections::HashSet::with_capacity(rows.len());
    let prices = rows
        .iter()
        .map(|row| {
            let slot = row["slot"].as_u64().filter(|slot| *slot > 0)?;
            if !slots.insert(slot) {
                return None;
            }
            row["prioritizationFee"].as_u64()
        })
        .collect::<Option<Vec<_>>>()?;
    percentile(prices)
}

pub async fn run(rpc: Rpc, fees: Fees, mut shutdown: watch::Receiver<bool>) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next_read = tokio::time::Instant::now();
    let mut fallback = None;
    let mut retry = 1u64;
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = timer.tick() => {},
        }
        let needs_fallback = fees.publish_current(now(), fallback).await;
        if needs_fallback && tokio::time::Instant::now() >= next_read {
            let value = tokio::select! {
                _ = shutdown.changed() => return,
                value = rpc.call("getRecentPrioritizationFees", json!([])) => value,
            };
            if let Some(price) = value.ok().as_ref().and_then(fallback_price) {
                fallback = Some((now(), price));
                retry = 30;
            }
            next_read = tokio::time::Instant::now() + Duration::from_secs(retry);
            retry = (retry * 2).min(30);
        }
        // recheck after network io so a fresh stream always wins over the fallback.
        fees.publish_current(now(), fallback).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(n: u64, slot: u64, at: u64, price: u64) -> Sample {
        let mut signature = [0; 64];
        signature[..8].copy_from_slice(&n.to_le_bytes());
        Sample {
            signature,
            slot,
            at,
            price,
        }
    }

    #[test]
    fn stream_estimates_require_fresh_distinct_transactions_across_slots() {
        let mut samples = Samples::default();
        for n in 0..8 {
            assert!(samples.insert(sample(n, 10, 100, n * 100), 100));
        }
        assert_eq!(samples.estimate(100), None);
        assert!(!samples.insert(sample(0, 11, 100, 99_000), 100));
        assert!(samples.insert(sample(8, 11, 100, 800), 100));
        assert_eq!(samples.estimate(100), Some(600));
        assert!(!samples.insert(sample(9, 12, 70, 1), 100));
        assert!(!samples.insert(sample(9, 12, 106, 1), 100));
        assert!(!samples.insert(sample(9, 0, 100, 1), 100));
        assert_eq!(samples.estimate(130), None);
        assert!(samples.values.is_empty());
    }

    #[test]
    fn stream_storage_is_bounded_and_zero_prices_remain_valid() {
        let mut samples = Samples::default();
        for n in 0..1_000 {
            samples.insert(sample(n, n + 1, 100, 0), 100);
        }
        assert_eq!(samples.values.len(), MAX_SAMPLES);
        assert_eq!(samples.estimate(100), Some(0));
    }

    #[test]
    fn fallback_prices_validate_bounds_slots_and_exact_unsigned_values() {
        assert_eq!(
            fallback_price(
                &json!([{"slot":1,"prioritizationFee":0},{"slot":2,"prioritizationFee":u64::MAX}])
            ),
            Some(0)
        );
        for value in [
            json!([]),
            json!([{"slot":1,"prioritizationFee":-1}]),
            json!([{"slot":0,"prioritizationFee":1}]),
            json!([{"slot":1,"prioritizationFee":1},{"slot":1,"prioritizationFee":2}]),
            json!(vec![json!({"slot":1,"prioritizationFee":1}); 257]),
        ] {
            assert_eq!(fallback_price(&value), None);
        }
    }

    #[tokio::test]
    async fn stream_publishes_once_per_second_and_wire_floors_stay_positive() {
        let fees = Fees::default();
        let mut updates = fees.updates.subscribe();
        for n in 0..8 {
            fees.observe_at(sample(n, n + 1, 100, 500), 100).await;
        }
        assert!(updates.has_changed().unwrap());
        assert_eq!(*updates.borrow_and_update(), 500);
        fees.observe_at(sample(8, 9, 100, 10_000), 100).await;
        assert!(!updates.has_changed().unwrap());
        assert_eq!(
            fees.snapshot(),
            json!({"baselineMicroLamports":500,"baselineFloorLamports":1000,"baselineCapLamports":10000})
        );
        assert_eq!(Fees::default().snapshot()["baselineFloorLamports"], 1000);
    }

    #[tokio::test]
    async fn stale_streams_switch_to_fresh_fallback_then_fee_floor() {
        let fees = Fees::default();
        for n in 0..8 {
            fees.observe_at(sample(n, n + 1, 100, 500), 100).await;
        }
        assert!(!fees.publish_current(100, Some((100, 700))).await);
        assert_eq!(fees.snapshot()["baselineMicroLamports"], 500);
        assert!(fees.publish_current(130, Some((100, 700))).await);
        assert_eq!(fees.snapshot()["baselineMicroLamports"], 700);
        assert!(fees.publish_current(220, Some((100, 700))).await);
        assert_eq!(fees.snapshot()["baselineMicroLamports"], 0);
        assert_eq!(fees.snapshot()["baselineFloorLamports"], 1000);
    }

    #[test]
    fn wire_prices_fit_javascript_integer_precision() {
        let fees = Fees::default();
        fees.publish(u64::MAX);
        assert_eq!(fees.snapshot()["baselineMicroLamports"], MAX_WIRE_PRICE);
        assert_eq!(fees.snapshot()["baselineCapLamports"], 10_000);
    }
}
