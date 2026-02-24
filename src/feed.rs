use std::time::Duration;

use crossbeam_channel::{tick, Receiver};

use crate::data::Candle;

#[derive(Debug, Clone, Copy)]
pub struct OrderBookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub mid_price: f64,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
}

impl OrderBookSnapshot {
    pub fn empty(mid_price: f64) -> Self {
        Self {
            mid_price,
            bids: Vec::new(),
            asks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeedEvent {
    pub chart_id: usize,
    pub candle: Candle,
    pub orderbook: OrderBookSnapshot,
}

pub fn start_mock_feed(chart_count: usize, interval: Duration) -> Receiver<FeedEvent> {
    let (tx, rx) = crossbeam_channel::bounded::<FeedEvent>(2_048);

    std::thread::spawn(move || {
        let ticker = tick(interval);
        let mut ts = 1_u64;
        let mut market = SyntheticMarket::new(chart_count);

        loop {
            if ticker.recv().is_err() {
                break;
            }

            for chart_id in 0..chart_count {
                let (candle, orderbook) = market.next_tick(chart_id, ts);
                let event = FeedEvent {
                    chart_id,
                    candle,
                    orderbook,
                };

                if tx.send(event).is_err() {
                    return;
                }
            }

            ts = ts.saturating_add(1);
        }
    });

    rx
}

#[derive(Debug)]
struct SyntheticMarket {
    series: Vec<SeriesState>,
    rng: XorShift64,
}

impl SyntheticMarket {
    fn new(chart_count: usize) -> Self {
        let mut rng = XorShift64::new(0x5EED_F00D_CAFE_BABE);
        let mut series = Vec::with_capacity(chart_count);
        for i in 0..chart_count {
            let start = 90.0 + (i as f64 * 30.0) + rng.next_f64() * 20.0;
            series.push(SeriesState::new(start));
        }
        Self { series, rng }
    }

    fn next_tick(&mut self, chart_id: usize, ts: u64) -> (Candle, OrderBookSnapshot) {
        let Some(state) = self.series.get_mut(chart_id) else {
            let fallback = Candle::synthetic(ts, 100.0);
            return (fallback, OrderBookSnapshot::empty(fallback.close));
        };

        if state.regime_ticks_left == 0 {
            state.reseed_regime(&mut self.rng);
        }
        state.regime_ticks_left = state.regime_ticks_left.saturating_sub(1);

        let open = state.last_close;
        let mut gap = 0.0_f64;
        if self.rng.next_f64() < 0.012 {
            gap = self.rng.range_f64(-0.015, 0.015);
        }
        let gap_open = (open * (1.0 + gap)).max(0.01);

        let drift = state.trend;
        let noise = self.rng.range_f64(-state.volatility, state.volatility);
        let close = (gap_open * (1.0 + drift + noise)).max(0.01);

        let wick_up = self.rng.range_f64(0.0, state.volatility * 2.2);
        let wick_down = self.rng.range_f64(0.0, state.volatility * 2.2);
        let mut high = gap_open.max(close) * (1.0 + wick_up);
        let mut low = gap_open.min(close) * (1.0 - wick_down);

        if self.rng.next_f64() < 0.01 {
            let spike = self.rng.range_f64(0.01, 0.035);
            if self.rng.next_bool() {
                high *= 1.0 + spike;
            } else {
                low *= 1.0 - spike;
            }
        }
        low = low.max(0.01);
        if high < low {
            high = low;
        }

        let mut volume = state.base_volume * (1.0 + self.rng.range_f64(-0.15, 0.15));
        if self.rng.next_f64() < 0.08 {
            volume *= self.rng.range_f64(1.6, 4.5);
        }
        volume = volume.max(1.0);

        state.last_close = close;
        let volatility = state.volatility;
        let base_volume = state.base_volume;

        let candle = Candle {
            ts,
            open: gap_open,
            high,
            low,
            close,
            volume,
        };
        let orderbook = self.generate_orderbook(close, volatility, base_volume);
        (candle, orderbook)
    }

    fn generate_orderbook(
        &mut self,
        mid: f64,
        volatility: f64,
        base_volume: f64,
    ) -> OrderBookSnapshot {
        let levels = 14_usize;
        let rel_spread =
            (0.00015 + volatility * 0.10 + self.rng.range_f64(0.0, 0.00035)).clamp(0.0001, 0.01);
        let spread = mid * rel_spread;
        let tick = (mid * rel_spread * 0.7).max(mid * 0.00005);
        let book_size_scale = (base_volume / 180.0).max(2.0);

        let mut bids = Vec::with_capacity(levels);
        let mut asks = Vec::with_capacity(levels);

        for level in 0..levels {
            let dist = level as f64;
            let price_ask = (mid + spread * 0.5 + dist * tick).max(0.01);
            let price_bid = (mid - spread * 0.5 - dist * tick).max(0.01);

            let decay = 1.0 / (1.0 + dist * 0.45);
            let skew = self.rng.range_f64(0.8, 1.2);
            let mut ask_size = book_size_scale * decay * skew;
            let mut bid_size = book_size_scale * decay * self.rng.range_f64(0.8, 1.2);

            if self.rng.next_f64() < 0.08 {
                ask_size *= self.rng.range_f64(1.5, 3.0);
            }
            if self.rng.next_f64() < 0.08 {
                bid_size *= self.rng.range_f64(1.5, 3.0);
            }

            asks.push(OrderBookLevel {
                price: price_ask,
                size: ask_size.max(0.1),
            });
            bids.push(OrderBookLevel {
                price: price_bid,
                size: bid_size.max(0.1),
            });
        }

        OrderBookSnapshot {
            mid_price: mid,
            bids,
            asks,
        }
    }
}

#[derive(Debug)]
struct SeriesState {
    last_close: f64,
    trend: f64,
    volatility: f64,
    regime_ticks_left: u32,
    base_volume: f64,
}

impl SeriesState {
    fn new(start_price: f64) -> Self {
        Self {
            last_close: start_price,
            trend: 0.0,
            volatility: 0.005,
            regime_ticks_left: 0,
            base_volume: 8_000.0,
        }
    }

    fn reseed_regime(&mut self, rng: &mut XorShift64) {
        self.regime_ticks_left = rng.range_u32(40, 220);
        self.trend = rng.range_f64(-0.0018, 0.0018);
        self.volatility = rng.range_f64(0.002, 0.02);
        self.base_volume = rng.range_f64(4_000.0, 35_000.0);
    }
}

#[derive(Debug)]
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0xA5A5_0123_89AB_CDEF
        } else {
            seed
        };
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_f64(&mut self) -> f64 {
        let v = self.next_u64() >> 11;
        (v as f64) * (1.0 / ((1_u64 << 53) as f64))
    }

    fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    fn range_f64(&mut self, min: f64, max: f64) -> f64 {
        min + (max - min) * self.next_f64()
    }

    fn range_u32(&mut self, min: u32, max: u32) -> u32 {
        if min >= max {
            return min;
        }
        min + (self.next_u64() % (u64::from(max - min) + 1)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::SyntheticMarket;

    #[test]
    fn synthetic_candles_respect_ohlc_invariants() {
        let mut market = SyntheticMarket::new(3);
        for ts in 1..=2_000 {
            for chart_id in 0..3 {
                let (c, _) = market.next_tick(chart_id, ts);
                assert!(c.low <= c.open);
                assert!(c.low <= c.close);
                assert!(c.high >= c.open);
                assert!(c.high >= c.close);
                assert!(c.low > 0.0);
                assert!(c.volume >= 1.0);
            }
        }
    }

    #[test]
    fn synthetic_orderbook_tracks_mid_and_is_sorted() {
        let mut market = SyntheticMarket::new(1);
        for ts in 1..=500 {
            let (c, ob) = market.next_tick(0, ts);
            assert!(ob.asks.len() >= 5);
            assert!(ob.bids.len() >= 5);
            assert!((ob.mid_price - c.close).abs() < 1e-9);

            for w in ob.asks.windows(2) {
                assert!(w[0].price < w[1].price);
            }
            for w in ob.bids.windows(2) {
                assert!(w[0].price > w[1].price);
            }
            assert!(ob.asks[0].price > ob.bids[0].price);
        }
    }
}
