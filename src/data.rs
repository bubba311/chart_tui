use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub struct Candle {
    pub ts: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl Candle {
    pub fn synthetic(ts: u64, base: f64) -> Self {
        let drift = ((ts % 20) as f64 - 10.0) * 0.0005;
        let open = base * (1.0 + drift);
        let close = base * (1.0 - drift * 0.8);
        let high = open.max(close) * 1.0015;
        let low = open.min(close) * 0.9985;
        Self {
            ts,
            open,
            high,
            low,
            close,
            volume: 1_000.0,
        }
    }
}

#[derive(Debug)]
pub struct CandleBuffer {
    capacity: usize,
    values: VecDeque<Candle>,
}

impl CandleBuffer {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            values: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, candle: Candle) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(candle);
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Candle> {
        self.values.get(index)
    }

    pub fn back_mut(&mut self) -> Option<&mut Candle> {
        self.values.back_mut()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PriceRange {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug)]
pub struct ChartState {
    candles: CandleBuffer,
    visible_start: usize,
    visible_len: usize,
    cached_range: Option<PriceRange>,
}

impl ChartState {
    pub fn new(capacity: usize, default_visible_len: usize) -> Self {
        Self {
            candles: CandleBuffer::with_capacity(capacity),
            visible_start: 0,
            visible_len: default_visible_len.max(1),
            cached_range: None,
        }
    }

    pub fn push(&mut self, candle: Candle) -> bool {
        let was_full = self.candles.len() == self.candles.capacity();
        let was_following_latest = self.is_following_latest();
        let old_start = self.visible_start;
        let old_len = self.visible_len;
        let old_count = self.candles.len();
        self.candles.push(candle);

        if was_full && self.visible_start > 0 {
            self.visible_start -= 1;
        }

        if was_following_latest {
            self.snap_to_latest();
        } else {
            self.clamp_visible_start();
        }

        old_count != self.candles.len()
            || old_start != self.visible_start
            || old_len != self.visible_len
    }

    pub fn len(&self) -> usize {
        self.candles.len()
    }

    pub fn last(&self) -> Option<&Candle> {
        self.candles
            .len()
            .checked_sub(1)
            .and_then(|idx| self.candles.get(idx))
    }

    pub fn replace_last(&mut self, candle: Candle) -> bool {
        if let Some(last) = self.candles.back_mut() {
            *last = candle;
            true
        } else {
            false
        }
    }

    pub fn visible_indices(&self) -> Option<(usize, usize)> {
        let len = self.candles.len();
        if len == 0 {
            return None;
        }

        let start = self.visible_start.min(len - 1);
        let visible = self.visible_len.min(len).max(1);
        let end = (start + visible).min(len);
        Some((start, end))
    }

    pub fn visible_count(&self) -> usize {
        self.visible_indices()
            .map(|(start, end)| end.saturating_sub(start))
            .unwrap_or(0)
    }

    pub fn fit_to_latest(&mut self, max_visible: usize) -> bool {
        let len = self.candles.len();
        if len == 0 {
            return false;
        }

        let target = len.min(max_visible.max(1));
        let old_start = self.visible_start;
        let old_len = self.visible_len;
        self.visible_len = target;
        self.visible_start = len.saturating_sub(target);
        old_start != self.visible_start || old_len != self.visible_len
    }

    pub fn get(&self, index: usize) -> Option<&Candle> {
        self.candles.get(index)
    }

    pub fn cached_range(&self) -> Option<PriceRange> {
        self.cached_range
    }

    pub fn pan(&mut self, delta: isize) -> bool {
        let len = self.candles.len();
        if len == 0 {
            return false;
        }

        let old_start = self.visible_start;
        let visible = self.visible_len.min(len).max(1);
        let max_start = len.saturating_sub(visible);
        let candidate = (self.visible_start as isize + delta).clamp(0, max_start as isize);
        self.visible_start = candidate as usize;
        old_start != self.visible_start
    }

    pub fn zoom(&mut self, zoom_in: bool) -> bool {
        let len = self.candles.len();
        if len == 0 {
            return false;
        }

        let old_start = self.visible_start;
        let old_len = self.visible_len;
        let current = self.visible_len.min(len).max(1);
        let step = (current / 10).max(1);
        let next = if zoom_in {
            current.saturating_sub(step).max(1)
        } else {
            (current + step).min(len)
        };

        let end = (self.visible_start + current).min(len);
        self.visible_len = next;
        self.visible_start = end.saturating_sub(self.visible_len);
        self.clamp_visible_start();
        old_start != self.visible_start || old_len != self.visible_len
    }

    pub fn map_price_to_row(&self, price: f64, height: u16) -> Option<u16> {
        if height == 0 {
            return None;
        }
        let range = self.cached_range?;
        if price <= 0.0 || range.min <= 0.0 || range.max <= 0.0 {
            return None;
        }

        let ln_min = range.min.ln();
        let ln_max = range.max.ln();
        let ln_price = price.ln();

        if (ln_max - ln_min).abs() < f64::EPSILON {
            return Some(height / 2);
        }

        let normalized = ((ln_max - ln_price) / (ln_max - ln_min)).clamp(0.0, 1.0);
        let row = (normalized * f64::from(height.saturating_sub(1))).round() as u16;
        Some(row)
    }

    pub fn map_index_to_col(&self, index: usize, width: u16) -> Option<u16> {
        if width == 0 {
            return None;
        }
        let (start, end) = self.visible_indices()?;
        if index < start || index >= end {
            return None;
        }

        let visible = end - start;
        let rel = index - start;
        let col = if visible <= 1 || width == 1 {
            0
        } else {
            (rel * usize::from(width - 1) / (visible - 1)) as u16
        };
        Some(col)
    }

    pub fn recompute_cached_range(&mut self) {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;

        if let Some((start, end)) = self.visible_indices() {
            for idx in start..end {
                if let Some(c) = self.candles.get(idx) {
                    min = min.min(c.low);
                    max = max.max(c.high);
                }
            }
        }

        self.cached_range = if min.is_finite() && max.is_finite() {
            Some(PriceRange { min, max })
        } else {
            None
        };
    }

    fn is_following_latest(&self) -> bool {
        let len = self.candles.len();
        if len == 0 {
            return true;
        }
        self.visible_start + self.visible_len >= len
    }

    fn snap_to_latest(&mut self) {
        let len = self.candles.len();
        if len == 0 {
            self.visible_start = 0;
            return;
        }
        let visible = self.visible_len.min(len).max(1);
        self.visible_start = len.saturating_sub(visible);
    }

    fn clamp_visible_start(&mut self) {
        let len = self.candles.len();
        if len == 0 {
            self.visible_start = 0;
            return;
        }

        let visible = self.visible_len.min(len).max(1);
        self.visible_start = self.visible_start.min(len.saturating_sub(visible));
    }
}

#[cfg(test)]
mod tests {
    use super::{Candle, CandleBuffer, ChartState};

    #[test]
    fn candle_buffer_is_fixed_capacity() {
        let mut buffer = CandleBuffer::with_capacity(2);
        buffer.push(Candle::synthetic(1, 100.0));
        buffer.push(Candle::synthetic(2, 101.0));
        buffer.push(Candle::synthetic(3, 102.0));

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.get(0).map(|c| c.ts), Some(2));
        assert_eq!(buffer.get(1).map(|c| c.ts), Some(3));
    }

    #[test]
    fn mapping_handles_flat_price_ranges() {
        let mut chart = ChartState::new(16, 8);
        let c = Candle {
            ts: 1,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 10.0,
        };
        chart.push(c);

        assert_eq!(chart.map_price_to_row(100.0, 10), None);
        chart.recompute_cached_range();
        assert_eq!(chart.map_price_to_row(100.0, 10), Some(5));
    }

    #[test]
    fn index_mapping_is_bounded_to_visible_window() {
        let mut chart = ChartState::new(16, 4);
        for i in 0..6 {
            chart.push(Candle::synthetic(i, 100.0 + i as f64));
        }

        let (start, end) = chart.visible_indices().expect("visible indices");
        assert_eq!(end - start, 4);
        assert!(chart
            .map_index_to_col(start.saturating_sub(1), 20)
            .is_none());
        assert!(chart.map_index_to_col(end, 20).is_none());
        assert!(chart.map_index_to_col(start, 20).is_some());
        assert!(chart.map_index_to_col(end - 1, 20).is_some());
    }
}
