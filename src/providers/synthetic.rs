use std::time::Duration;

use crossbeam_channel::Receiver;

use crate::feed::{self, FeedEvent};

use super::{MarketDataProvider, MarketEvent};

pub struct SyntheticProvider {
    interval: Duration,
    symbols: Vec<String>,
    rx: Option<Receiver<FeedEvent>>,
}

impl SyntheticProvider {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            symbols: Vec::new(),
            rx: None,
        }
    }

    fn restart_stream(&mut self) {
        if self.symbols.is_empty() {
            self.rx = None;
            return;
        }
        self.rx = Some(feed::start_mock_feed(self.symbols.len(), self.interval));
    }
}

impl MarketDataProvider for SyntheticProvider {
    fn name(&self) -> &'static str {
        "synthetic"
    }

    fn subscribe(&mut self, symbols: Vec<String>) {
        if self.symbols == symbols {
            return;
        }
        self.symbols = symbols;
        self.restart_stream();
    }

    fn poll_events(&mut self) -> Vec<MarketEvent> {
        let mut out = Vec::new();
        let Some(rx) = self.rx.as_ref() else {
            return out;
        };

        while let Ok(event) = rx.try_recv() {
            let Some(symbol) = self.symbols.get(event.chart_id) else {
                continue;
            };
            out.push(MarketEvent {
                symbol: symbol.clone(),
                candle: event.candle,
                orderbook: event.orderbook,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::SyntheticProvider;
    use crate::providers::MarketDataProvider;

    #[test]
    fn provider_emits_subscribed_symbols() {
        let mut provider = SyntheticProvider::new(std::time::Duration::from_millis(1));
        provider.subscribe(vec!["AAPL".to_string(), "MSFT".to_string()]);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let events = provider.poll_events();
        assert!(!events.is_empty());
        assert!(events
            .iter()
            .all(|e| e.symbol == "AAPL" || e.symbol == "MSFT"));
    }
}
