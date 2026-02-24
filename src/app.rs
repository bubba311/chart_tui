use std::time::Duration;

use crossbeam_channel::Receiver;
use smallvec::SmallVec;

use crate::{
    data::{Candle, CandleBuffer, ChartState},
    feed::{FeedEvent, OrderBookSnapshot},
    input::UserAction,
};

pub const TARGET_FPS: u64 = 30;
pub const CHART_BUFFER_CAPACITY: usize = 2_000;
pub const SOURCE_BUFFER_CAPACITY: usize = 10_000;
pub const DEFAULT_VISIBLE_CANDLES: usize = 300;
pub const TARGET_FRAME_TIME: Duration = Duration::from_millis(1_000 / TARGET_FPS);
pub const MAX_VISIBLE_PANES: usize = 4;
pub const SINGLE_LAYOUT_THRESHOLD: usize = 900;
pub const TWO_LAYOUT_THRESHOLD: usize = 450;
pub const QUAD_LAYOUT_THRESHOLD: usize = 220;
pub const SYMBOL_UNIVERSE: [&str; 12] = [
    "AAPL", "MSFT", "TSLA", "NVDA", "AMZN", "META", "GOOGL", "AMD", "NFLX", "PLTR", "COIN", "SPY",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Single,
    TwoUp,
    Quad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeframe {
    M1,
    M3,
    M15,
    M45,
    H1,
    H3,
    H4,
    D1,
}

impl Timeframe {
    pub const ALL: [Timeframe; 8] = [
        Timeframe::M1,
        Timeframe::M3,
        Timeframe::M15,
        Timeframe::M45,
        Timeframe::H1,
        Timeframe::H3,
        Timeframe::H4,
        Timeframe::D1,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Timeframe::M1 => "1m",
            Timeframe::M3 => "3m",
            Timeframe::M15 => "15m",
            Timeframe::M45 => "45m",
            Timeframe::H1 => "1h",
            Timeframe::H3 => "3h",
            Timeframe::H4 => "4h",
            Timeframe::D1 => "1d",
        }
    }

    pub fn minutes(self) -> u64 {
        match self {
            Timeframe::M1 => 1,
            Timeframe::M3 => 3,
            Timeframe::M15 => 15,
            Timeframe::M45 => 45,
            Timeframe::H1 => 60,
            Timeframe::H3 => 180,
            Timeframe::H4 => 240,
            Timeframe::D1 => 1_440,
        }
    }
}

#[derive(Debug)]
pub struct ChartPane {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub chart: ChartState,
    source: CandleBuffer,
    current_bucket: Option<u64>,
    auto_fit: bool,
    pub latest_orderbook: Option<OrderBookSnapshot>,
    price_scale: Option<f64>,
}

#[derive(Debug)]
pub struct App {
    pub panes: Vec<ChartPane>,
    pub active_pane: usize,
    pub layout: LayoutMode,
    pub show_orderbook: bool,
    pub ticker_input: String,
    pub ticker_entry_active: bool,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pane_dirty: Vec<bool>,
}

impl App {
    pub fn new() -> Self {
        let symbols = ["AAPL", "MSFT", "TSLA", "NVDA"];
        let panes: Vec<ChartPane> = symbols
            .iter()
            .map(|symbol| ChartPane {
                symbol: (*symbol).to_string(),
                timeframe: Timeframe::M1,
                chart: ChartState::new(CHART_BUFFER_CAPACITY, DEFAULT_VISIBLE_CANDLES),
                source: CandleBuffer::with_capacity(SOURCE_BUFFER_CAPACITY),
                current_bucket: None,
                auto_fit: true,
                latest_orderbook: None,
                price_scale: None,
            })
            .collect();

        Self {
            pane_dirty: vec![true; panes.len()],
            panes,
            active_pane: 0,
            layout: LayoutMode::Single,
            show_orderbook: false,
            ticker_input: String::new(),
            ticker_entry_active: false,
            status_message: None,
            should_quit: false,
        }
    }

    pub fn handle_action(&mut self, action: UserAction) {
        match action {
            UserAction::Quit => self.should_quit = true,
            UserAction::SetLayoutSingle => {
                self.layout = LayoutMode::Single;
                self.apply_layout_thresholds();
            }
            UserAction::SetLayoutTwo => {
                self.layout = LayoutMode::TwoUp;
                self.apply_layout_thresholds();
            }
            UserAction::SetLayoutQuad => {
                self.layout = LayoutMode::Quad;
                self.apply_layout_thresholds();
            }
            UserAction::ToggleOrderBook => {
                self.show_orderbook = !self.show_orderbook;
            }
            UserAction::PrevTicker => self.shift_ticker(false),
            UserAction::NextTicker => self.shift_ticker(true),
            UserAction::RawChar(c) => self.handle_raw_char(c),
            UserAction::TickerBackspace => {
                if self.ticker_entry_active {
                    self.ticker_input.pop();
                }
            }
            UserAction::TickerCancel => {
                self.ticker_input.clear();
                self.ticker_entry_active = false;
                self.status_message = None;
            }
            UserAction::TickerSubmit => {
                if self.ticker_entry_active {
                    self.submit_ticker_input();
                }
            }
            UserAction::PrevTimeframe => self.shift_timeframe(false),
            UserAction::NextTimeframe => self.shift_timeframe(true),
            UserAction::NextPane => {
                let pane_count = self.panes.len();
                if pane_count > 0 {
                    self.active_pane = (self.active_pane + 1) % pane_count;
                } else {
                    self.active_pane = 0;
                }
            }
            UserAction::PanLeft
            | UserAction::PanRight
            | UserAction::ZoomIn
            | UserAction::ZoomOut => {
                let active_idx = self.active_pane;
                let changed = if let Some(chart) = self.active_chart_mut() {
                    let changed = match action {
                        UserAction::PanLeft => chart.pan(-10),
                        UserAction::PanRight => chart.pan(10),
                        UserAction::ZoomIn => chart.zoom(true),
                        UserAction::ZoomOut => chart.zoom(false),
                        UserAction::Quit
                        | UserAction::NextPane
                        | UserAction::SetLayoutSingle
                        | UserAction::SetLayoutTwo
                        | UserAction::SetLayoutQuad
                        | UserAction::ToggleOrderBook
                        | UserAction::PrevTicker
                        | UserAction::NextTicker
                        | UserAction::RawChar(_)
                        | UserAction::TickerBackspace
                        | UserAction::TickerSubmit
                        | UserAction::TickerCancel
                        | UserAction::PrevTimeframe
                        | UserAction::NextTimeframe => false,
                    };
                    changed
                } else {
                    false
                };
                if changed {
                    if let Some(pane) = self.panes.get_mut(active_idx) {
                        pane.auto_fit = false;
                    }
                    self.mark_pane_dirty(active_idx);
                }
            }
        }
    }

    pub fn visible_pane_indices(&self) -> SmallVec<[usize; MAX_VISIBLE_PANES]> {
        let pane_count = self.panes.len();
        if pane_count == 0 {
            return SmallVec::new();
        }

        let mut out = SmallVec::new();
        match self.layout {
            LayoutMode::Single => {
                out.push(self.active_pane.min(pane_count - 1));
            }
            LayoutMode::TwoUp => {
                let first = self.active_pane.min(pane_count - 1);
                out.push(first);
                if pane_count > 1 {
                    out.push((first + 1) % pane_count);
                }
            }
            LayoutMode::Quad => {
                for idx in 0..pane_count.min(MAX_VISIBLE_PANES) {
                    out.push(idx);
                }
            }
        }
        out
    }

    pub fn drain_feed(&mut self, rx: &Receiver<FeedEvent>) -> usize {
        let mut updated = 0_usize;
        let threshold = self.layout_threshold();
        while let Ok(event) = rx.try_recv() {
            let mut changed = false;
            if let Some(pane) = self.panes.get_mut(event.chart_id) {
                let (scaled_candle, scaled_orderbook) = scale_feed_to_symbol(pane, &event);
                pane.source.push(scaled_candle);
                pane.latest_orderbook = Some(scaled_orderbook);
                if apply_base_candle_to_pane(pane, scaled_candle) {
                    changed = true;
                }
                if pane.auto_fit && pane.chart.fit_to_latest(threshold) {
                    changed = true;
                }
                updated += 1;
            }
            if changed {
                self.mark_pane_dirty(event.chart_id);
            }
        }
        updated
    }

    fn active_chart_mut(&mut self) -> Option<&mut ChartState> {
        self.panes
            .get_mut(self.active_pane)
            .map(|pane| &mut pane.chart)
    }

    fn shift_timeframe(&mut self, forward: bool) {
        let idx = self.active_pane;
        let threshold = self.layout_threshold();
        let Some(pane) = self.panes.get_mut(idx) else {
            return;
        };

        let pos = Timeframe::ALL
            .iter()
            .position(|tf| *tf == pane.timeframe)
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % Timeframe::ALL.len()
        } else {
            (pos + Timeframe::ALL.len() - 1) % Timeframe::ALL.len()
        };

        pane.timeframe = Timeframe::ALL[next];
        rebuild_aggregated_chart(pane);
        pane.auto_fit = true;
        let _ = pane.chart.fit_to_latest(threshold);
        self.mark_pane_dirty(idx);
    }

    fn shift_ticker(&mut self, forward: bool) {
        let idx = self.active_pane;
        let Some(pane) = self.panes.get_mut(idx) else {
            return;
        };

        let pos = SYMBOL_UNIVERSE
            .iter()
            .position(|s| *s == pane.symbol.as_str())
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % SYMBOL_UNIVERSE.len()
        } else {
            (pos + SYMBOL_UNIVERSE.len() - 1) % SYMBOL_UNIVERSE.len()
        };

        pane.symbol = SYMBOL_UNIVERSE[next].to_string();
        reset_pane_stream_state(pane);
        self.status_message = Some(format!("Switched to {}", pane.symbol));
        self.mark_pane_dirty(idx);
    }

    fn submit_ticker_input(&mut self) {
        if self.ticker_input.is_empty() {
            self.ticker_entry_active = false;
            return;
        }

        let symbol = self.ticker_input.to_ascii_uppercase();
        if SYMBOL_UNIVERSE.contains(&symbol.as_str()) {
            let idx = self.active_pane;
            if let Some(pane) = self.panes.get_mut(idx) {
                pane.symbol = symbol.clone();
                reset_pane_stream_state(pane);
                self.status_message = Some(format!("Switched to {}", symbol));
                self.mark_pane_dirty(idx);
            }
        } else {
            self.status_message = Some(format!("No data stream for {}", symbol));
        }
        self.ticker_input.clear();
        self.ticker_entry_active = false;
    }

    fn handle_raw_char(&mut self, c: char) {
        if self.ticker_entry_active {
            if c.is_ascii_alphanumeric() && self.ticker_input.len() < 8 {
                self.ticker_input.push(c.to_ascii_uppercase());
            }
            return;
        }

        match c {
            'q' | 'Q' => self.should_quit = true,
            '1' => {
                self.layout = LayoutMode::Single;
                self.apply_layout_thresholds();
            }
            '2' => {
                self.layout = LayoutMode::TwoUp;
                self.apply_layout_thresholds();
            }
            '4' => {
                self.layout = LayoutMode::Quad;
                self.apply_layout_thresholds();
            }
            'o' | 'O' => {
                self.show_orderbook = !self.show_orderbook;
            }
            't' | 'T' => {
                self.ticker_entry_active = true;
                self.ticker_input.clear();
                self.status_message = Some("Enter ticker and press Enter".to_string());
            }
            ',' => self.shift_ticker(false),
            '.' => self.shift_ticker(true),
            '[' => self.shift_timeframe(false),
            ']' => self.shift_timeframe(true),
            '+' | '=' => {
                if let Some(pane) = self.panes.get_mut(self.active_pane) {
                    if pane.chart.zoom(true) {
                        pane.auto_fit = false;
                        self.mark_pane_dirty(self.active_pane);
                    }
                }
            }
            '-' | '_' => {
                if let Some(pane) = self.panes.get_mut(self.active_pane) {
                    if pane.chart.zoom(false) {
                        pane.auto_fit = false;
                        self.mark_pane_dirty(self.active_pane);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn refresh_dirty_stats(&mut self) {
        self.sync_dirty_len();
        for (idx, dirty) in self.pane_dirty.iter_mut().enumerate() {
            if !*dirty {
                continue;
            }
            if let Some(pane) = self.panes.get_mut(idx) {
                pane.chart.recompute_cached_range();
            }
            *dirty = false;
        }
    }

    fn mark_pane_dirty(&mut self, pane_idx: usize) {
        self.sync_dirty_len();
        if let Some(dirty) = self.pane_dirty.get_mut(pane_idx) {
            *dirty = true;
        }
    }

    fn sync_dirty_len(&mut self) {
        if self.pane_dirty.len() != self.panes.len() {
            self.pane_dirty.resize(self.panes.len(), true);
        }
    }

    fn apply_layout_thresholds(&mut self) {
        let threshold = self.layout_threshold();
        for idx in 0..self.panes.len() {
            let changed = self
                .panes
                .get_mut(idx)
                .map(|pane| {
                    pane.auto_fit = true;
                    pane.chart.fit_to_latest(threshold)
                })
                .unwrap_or(false);
            if changed {
                self.mark_pane_dirty(idx);
            }
        }
    }

    fn layout_threshold(&self) -> usize {
        match self.layout {
            LayoutMode::Single => SINGLE_LAYOUT_THRESHOLD,
            LayoutMode::TwoUp => TWO_LAYOUT_THRESHOLD,
            LayoutMode::Quad => QUAD_LAYOUT_THRESHOLD,
        }
    }
}

fn scale_feed_to_symbol(pane: &mut ChartPane, event: &FeedEvent) -> (Candle, OrderBookSnapshot) {
    let target = symbol_reference_price(&pane.symbol);
    let source_close = event.candle.close.max(0.01);
    let scale = pane.price_scale.unwrap_or_else(|| {
        let s = (target / source_close).clamp(0.05, 100.0);
        pane.price_scale = Some(s);
        s
    });

    let mut candle = event.candle;
    candle.open = (candle.open * scale).max(0.01);
    candle.high = (candle.high * scale).max(0.01);
    candle.low = (candle.low * scale).max(0.01);
    candle.close = (candle.close * scale).max(0.01);

    let mut orderbook = event.orderbook.clone();
    orderbook.mid_price = (orderbook.mid_price * scale).max(0.01);
    for level in &mut orderbook.asks {
        level.price = (level.price * scale).max(0.01);
    }
    for level in &mut orderbook.bids {
        level.price = (level.price * scale).max(0.01);
    }

    (candle, orderbook)
}

fn symbol_reference_price(symbol: &str) -> f64 {
    match symbol {
        "AAPL" => 190.0,
        "MSFT" => 420.0,
        "TSLA" => 230.0,
        "NVDA" => 900.0,
        "AMZN" => 180.0,
        "META" => 520.0,
        "GOOGL" => 180.0,
        "AMD" => 190.0,
        "NFLX" => 650.0,
        "PLTR" => 28.0,
        "COIN" => 240.0,
        "SPY" => 520.0,
        _ => 100.0,
    }
}

fn candle_bucket(ts: u64, timeframe: Timeframe) -> u64 {
    let span = timeframe.minutes();
    (ts / span) * span
}

fn apply_base_candle_to_pane(pane: &mut ChartPane, base: Candle) -> bool {
    let bucket = candle_bucket(base.ts, pane.timeframe);

    if pane.current_bucket == Some(bucket) {
        if let Some(last) = pane.chart.last().copied() {
            let merged = Candle {
                ts: bucket,
                open: last.open,
                high: last.high.max(base.high),
                low: last.low.min(base.low),
                close: base.close,
                volume: last.volume + base.volume,
            };
            return pane.chart.replace_last(merged);
        }
    }

    pane.current_bucket = Some(bucket);
    let aggregated = Candle {
        ts: bucket,
        open: base.open,
        high: base.high,
        low: base.low,
        close: base.close,
        volume: base.volume,
    };
    pane.chart.push(aggregated)
}

fn rebuild_aggregated_chart(pane: &mut ChartPane) {
    pane.chart = ChartState::new(CHART_BUFFER_CAPACITY, DEFAULT_VISIBLE_CANDLES);
    pane.current_bucket = None;

    let source_len = pane.source.len();
    for i in 0..source_len {
        if let Some(base) = pane.source.get(i).copied() {
            let _ = apply_base_candle_to_pane(pane, base);
        }
    }
}

fn reset_pane_stream_state(pane: &mut ChartPane) {
    pane.chart = ChartState::new(CHART_BUFFER_CAPACITY, DEFAULT_VISIBLE_CANDLES);
    pane.source = CandleBuffer::with_capacity(SOURCE_BUFFER_CAPACITY);
    pane.current_bucket = None;
    pane.latest_orderbook = None;
    pane.price_scale = None;
    pane.auto_fit = true;
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use crate::{
        data::{Candle, CandleBuffer, ChartState},
        feed::{FeedEvent, OrderBookSnapshot},
        input::UserAction,
    };

    use super::{
        apply_base_candle_to_pane, App, ChartPane, LayoutMode, Timeframe, DEFAULT_VISIBLE_CANDLES,
        QUAD_LAYOUT_THRESHOLD, TWO_LAYOUT_THRESHOLD,
    };

    #[test]
    fn default_layout_is_single() {
        let app = App::new();
        assert_eq!(app.layout, LayoutMode::Single);
    }

    #[test]
    fn next_pane_wraps_across_all_panes() {
        let mut app = App::new();

        for expected in 1..app.panes.len() {
            app.handle_action(UserAction::NextPane);
            assert_eq!(app.active_pane, expected);
        }

        app.handle_action(UserAction::NextPane);
        assert_eq!(app.active_pane, 0);
    }

    #[test]
    fn drain_feed_updates_non_active_panes() {
        let mut app = App::new();
        app.active_pane = 0;
        let (tx, rx) = unbounded();

        tx.send(FeedEvent {
            chart_id: 2,
            candle: Candle::synthetic(1, 200.0),
            orderbook: OrderBookSnapshot::empty(200.0),
        })
        .expect("send feed event");
        tx.send(FeedEvent {
            chart_id: 3,
            candle: Candle::synthetic(2, 300.0),
            orderbook: OrderBookSnapshot::empty(300.0),
        })
        .expect("send feed event");

        let updated = app.drain_feed(&rx);

        assert_eq!(updated, 2);
        assert_eq!(app.panes[0].chart.len(), 0);
        assert_eq!(app.panes[2].chart.len(), 1);
        assert_eq!(app.panes[3].chart.len(), 1);
    }

    #[test]
    fn pan_and_zoom_apply_only_to_active_pane() {
        let mut app = App::new();
        let (tx, rx) = unbounded();

        for i in 0..200 {
            tx.send(FeedEvent {
                chart_id: 1,
                candle: Candle::synthetic(i, 100.0 + i as f64),
                orderbook: OrderBookSnapshot::empty(100.0 + i as f64),
            })
            .expect("send feed event");
        }
        app.drain_feed(&rx);
        app.active_pane = 1;

        let before = app.panes[1].chart.visible_indices();
        app.handle_action(UserAction::PanLeft);
        app.handle_action(UserAction::ZoomIn);
        let after = app.panes[1].chart.visible_indices();

        assert_ne!(before, after);
        assert_eq!(app.panes[0].chart.len(), 0);
        assert_eq!(app.panes[2].chart.len(), 0);
        assert_eq!(app.panes[3].chart.len(), 0);
    }

    #[test]
    fn visible_panes_follow_selected_layout() {
        let mut app = App::new();
        app.active_pane = 2;
        assert_eq!(app.visible_pane_indices().as_slice(), &[2]);

        app.handle_action(UserAction::SetLayoutTwo);
        assert_eq!(app.visible_pane_indices().as_slice(), &[2, 3]);

        app.active_pane = 3;
        assert_eq!(app.visible_pane_indices().as_slice(), &[3, 0]);

        app.handle_action(UserAction::SetLayoutQuad);
        assert_eq!(app.visible_pane_indices().as_slice(), &[0, 1, 2, 3]);
    }

    #[test]
    fn visible_panes_are_bounded_when_pane_count_is_small() {
        let mut app = App::new();
        app.panes.truncate(1);
        app.active_pane = 0;

        app.handle_action(UserAction::SetLayoutTwo);
        assert_eq!(app.visible_pane_indices().as_slice(), &[0]);

        app.handle_action(UserAction::SetLayoutQuad);
        assert_eq!(app.visible_pane_indices().as_slice(), &[0]);
    }

    #[test]
    fn refresh_dirty_stats_recomputes_ranges_only_after_updates() {
        let mut app = App::new();
        let (tx, rx) = unbounded();

        tx.send(FeedEvent {
            chart_id: 0,
            candle: Candle::synthetic(1, 100.0),
            orderbook: OrderBookSnapshot::empty(100.0),
        })
        .expect("send feed event");
        app.drain_feed(&rx);

        assert!(app.panes[0].chart.cached_range().is_none());
        app.refresh_dirty_stats();
        assert!(app.panes[0].chart.cached_range().is_some());
    }

    #[test]
    fn higher_timeframe_updates_last_candle_in_place() {
        let mut pane = ChartPane {
            symbol: "TEST".to_string(),
            timeframe: Timeframe::M3,
            chart: ChartState::new(128, DEFAULT_VISIBLE_CANDLES),
            source: CandleBuffer::with_capacity(256),
            current_bucket: None,
            auto_fit: true,
            latest_orderbook: None,
            price_scale: None,
        };

        let c1 = Candle {
            ts: 10,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 10.0,
        };
        let c2 = Candle {
            ts: 11,
            open: 100.5,
            high: 102.0,
            low: 98.0,
            close: 101.0,
            volume: 12.0,
        };

        assert!(apply_base_candle_to_pane(&mut pane, c1));
        assert_eq!(pane.chart.len(), 1);
        assert!(apply_base_candle_to_pane(&mut pane, c2));
        assert_eq!(pane.chart.len(), 1);

        let last = pane.chart.last().expect("last candle");
        assert_eq!(last.ts, 9);
        assert_eq!(last.open, 100.0);
        assert_eq!(last.high, 102.0);
        assert_eq!(last.low, 98.0);
        assert_eq!(last.close, 101.0);
        assert_eq!(last.volume, 22.0);
    }

    #[test]
    fn timeframe_switch_rebuilds_from_source_history() {
        let mut app = App::new();
        let (tx, rx) = unbounded();

        for ts in 1..=6 {
            tx.send(FeedEvent {
                chart_id: 0,
                candle: Candle {
                    ts,
                    open: 100.0 + ts as f64,
                    high: 101.0 + ts as f64,
                    low: 99.0 + ts as f64,
                    close: 100.5 + ts as f64,
                    volume: 10.0,
                },
                orderbook: OrderBookSnapshot::empty(100.5 + ts as f64),
            })
            .expect("send feed event");
        }
        app.drain_feed(&rx);
        app.active_pane = 0;
        assert_eq!(app.panes[0].timeframe, Timeframe::M1);
        assert_eq!(app.panes[0].chart.len(), 6);

        app.handle_action(UserAction::NextTimeframe); // 3m
        assert_eq!(app.panes[0].timeframe, Timeframe::M3);
        assert_eq!(app.panes[0].chart.len(), 3);
    }

    #[test]
    fn layout_threshold_caps_visible_window() {
        let mut app = App::new();
        let (tx, rx) = unbounded();

        for ts in 1..=600 {
            tx.send(FeedEvent {
                chart_id: 0,
                candle: Candle::synthetic(ts, 100.0 + ts as f64 * 0.01),
                orderbook: OrderBookSnapshot::empty(100.0 + ts as f64 * 0.01),
            })
            .expect("send feed event");
        }
        app.drain_feed(&rx);
        assert_eq!(app.panes[0].chart.visible_count(), 600);

        app.handle_action(UserAction::SetLayoutTwo);
        assert_eq!(app.panes[0].chart.visible_count(), TWO_LAYOUT_THRESHOLD);

        app.handle_action(UserAction::SetLayoutQuad);
        assert_eq!(app.panes[0].chart.visible_count(), QUAD_LAYOUT_THRESHOLD);
    }

    #[test]
    fn ticker_switch_resets_active_pane_stream_state() {
        let mut app = App::new();
        let (tx, rx) = unbounded();

        tx.send(FeedEvent {
            chart_id: 0,
            candle: Candle::synthetic(1, 150.0),
            orderbook: OrderBookSnapshot::empty(150.0),
        })
        .expect("send feed event");
        app.drain_feed(&rx);
        assert!(app.panes[0].chart.len() > 0);
        assert!(app.panes[0].latest_orderbook.is_some());

        let old_symbol = app.panes[0].symbol.clone();
        app.handle_action(UserAction::NextTicker);
        assert_ne!(app.panes[0].symbol, old_symbol);
        assert_eq!(app.panes[0].chart.len(), 0);
        assert!(app.panes[0].latest_orderbook.is_none());
    }

    #[test]
    fn ticker_submit_accepts_known_symbol() {
        let mut app = App::new();
        app.ticker_entry_active = true;
        app.ticker_input = "AMD".to_string();
        app.handle_action(UserAction::TickerSubmit);
        assert_eq!(app.panes[0].symbol, "AMD");
        assert!(app
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("Switched"));
    }

    #[test]
    fn ticker_submit_rejects_unknown_symbol_without_switch() {
        let mut app = App::new();
        let before = app.panes[0].symbol.clone();
        app.ticker_entry_active = true;
        app.ticker_input = "ZZZZ".to_string();
        app.handle_action(UserAction::TickerSubmit);
        assert_eq!(app.panes[0].symbol, before);
        assert!(app
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("No data stream"));
    }

    #[test]
    fn ticker_entry_starts_only_after_t_key() {
        let mut app = App::new();
        app.handle_action(UserAction::RawChar('A'));
        assert!(app.ticker_input.is_empty());

        app.handle_action(UserAction::RawChar('t'));
        assert!(app.ticker_entry_active);
        app.handle_action(UserAction::RawChar('A'));
        app.handle_action(UserAction::RawChar('M'));
        app.handle_action(UserAction::RawChar('D'));
        assert_eq!(app.ticker_input, "AMD");
    }
}
