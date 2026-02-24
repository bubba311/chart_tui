use std::time::Duration;

use crossbeam_channel::{tick, Receiver};

use crate::data::Candle;

#[derive(Debug, Clone)]
pub struct FeedEvent {
    pub chart_id: usize,
    pub candle: Candle,
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
                let candle = market.next_candle(chart_id, ts);
                let event = FeedEvent { chart_id, candle };

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

    fn next_candle(&mut self, chart_id: usize, ts: u64) -> Candle {
        let Some(state) = self.series.get_mut(chart_id) else {
            return Candle::synthetic(ts, 100.0);
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

        Candle {
            ts,
            open: gap_open,
            high,
            low,
            close,
            volume,
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
                let c = market.next_candle(chart_id, ts);
                assert!(c.low <= c.open);
                assert!(c.low <= c.close);
                assert!(c.high >= c.open);
                assert!(c.high >= c.close);
                assert!(c.low > 0.0);
                assert!(c.volume >= 1.0);
            }
        }
    }
}
