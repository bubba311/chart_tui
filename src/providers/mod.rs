use crate::data::Candle;
use crate::feed::OrderBookSnapshot;

pub mod schwab;
pub mod synthetic;

#[derive(Debug, Clone)]
pub struct MarketEvent {
    pub symbol: String,
    pub candle: Candle,
    pub orderbook: OrderBookSnapshot,
}

pub trait MarketDataProvider {
    fn name(&self) -> &'static str;
    fn subscribe(&mut self, symbols: Vec<String>);
    fn poll_events(&mut self) -> Vec<MarketEvent>;
}

pub trait OAuthProvider {
    fn authorization_url(&self, state: &str) -> Result<String, String>;
    fn token_url(&self) -> &'static str;
}
