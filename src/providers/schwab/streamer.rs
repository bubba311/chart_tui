use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::net::TcpStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::America::New_York;
use crossbeam_channel::{unbounded, Receiver, Sender};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING};
use serde::Deserialize;
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Error as WsError, Message, WebSocket};

use crate::data::Candle;
use crate::feed::{OrderBookLevel, OrderBookSnapshot};
use crate::providers::synthetic::SyntheticProvider;
use crate::providers::{MarketDataProvider, MarketEvent};

use super::{
    default_token_file_path, load_tokens, save_tokens, SchwabOAuth, SchwabOAuthConfig, StoredTokens,
};

const USER_PREFERENCE_URL: &str = "https://api.schwabapi.com/trader/v1/userPreference";
const PRICE_HISTORY_URL: &str = "https://api.schwabapi.com/marketdata/v1/pricehistory";
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const IO_POLL_SLEEP: Duration = Duration::from_millis(20);
const BOOK_DEPTH: usize = 14;
const ACCESS_TOKEN_REFRESH_SECONDS: u64 = 29 * 60;
const REFRESH_RETRY_THROTTLE_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchwabService {
    LevelOneEquities,
    LevelOneOptions,
    LevelOneFutures,
    LevelOneFuturesOptions,
    LevelOneForex,
    ChartEquity,
    ChartFutures,
    NyseBook,
    NasdaqBook,
    OptionsBook,
}

impl SchwabService {
    pub fn as_str(self) -> &'static str {
        match self {
            SchwabService::LevelOneEquities => "LEVELONE_EQUITIES",
            SchwabService::LevelOneOptions => "LEVELONE_OPTIONS",
            SchwabService::LevelOneFutures => "LEVELONE_FUTURES",
            SchwabService::LevelOneFuturesOptions => "LEVELONE_FUTURES_OPTIONS",
            SchwabService::LevelOneForex => "LEVELONE_FOREX",
            SchwabService::ChartEquity => "CHART_EQUITY",
            SchwabService::ChartFutures => "CHART_FUTURES",
            SchwabService::NyseBook => "NYSE_BOOK",
            SchwabService::NasdaqBook => "NASDAQ_BOOK",
            SchwabService::OptionsBook => "OPTIONS_BOOK",
        }
    }

    pub fn default_fields(self) -> &'static str {
        match self {
            SchwabService::LevelOneEquities => "0,1,2,3,4,5,8,9,10,35,36",
            SchwabService::LevelOneOptions => "0,1,2,3,16,17,8,9,10,35,36",
            SchwabService::LevelOneFutures => "0,1,2,3,4,5,8,9,10,35,36",
            SchwabService::LevelOneFuturesOptions => "0,1,2,3,4,5,8,9,10,35,36",
            SchwabService::LevelOneForex => "0,1,2,3,4,5,8,9,10,35,36",
            SchwabService::ChartEquity => "0,1,2,3,4,5,6,7,8",
            SchwabService::ChartFutures => "0,1,2,3,4,5,6",
            SchwabService::NyseBook => "0,1,2,3",
            SchwabService::NasdaqBook => "0,1,2,3",
            SchwabService::OptionsBook => "0,1,2,3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Equity,
    Option,
    Future,
    FutureOption,
    Forex,
}

pub fn infer_symbol_kind(symbol: &str) -> SymbolKind {
    let s = symbol.trim();
    if s.starts_with('/') {
        if s.contains('C') || s.contains('P') {
            return SymbolKind::FutureOption;
        }
        return SymbolKind::Future;
    }

    if s.contains('/') && !s.contains(' ') {
        return SymbolKind::Forex;
    }

    if looks_like_option_symbol(s) {
        return SymbolKind::Option;
    }

    if looks_like_futures_symbol(s) {
        return SymbolKind::Future;
    }

    SymbolKind::Equity
}

fn looks_like_option_symbol(s: &str) -> bool {
    let t = s.trim();
    if t.len() < 16 {
        return false;
    }

    if t.contains(' ') {
        return t.len() >= 21 && (t.contains('C') || t.contains('P'));
    }

    let n = t.len();
    if n <= 15 {
        return false;
    }
    let tail = &t[n - 15..];
    let cp = tail.as_bytes()[6] as char;
    cp == 'C' || cp == 'P'
}

fn looks_like_futures_symbol(s: &str) -> bool {
    let t = s.trim().to_ascii_uppercase();
    let bytes = t.as_bytes();
    if bytes.len() < 4 {
        return false;
    }

    let month_codes = b"FGHJKMNQUVXZ";
    for i in 1..bytes.len().saturating_sub(1) {
        let month = bytes[i];
        if !month_codes.contains(&month) {
            continue;
        }
        let root = &bytes[..i];
        let year = &bytes[i + 1..];
        if root.is_empty() || !root.iter().all(|b| b.is_ascii_alphabetic()) {
            continue;
        }
        if year.is_empty() || year.len() > 4 || !year.iter().all(|b| b.is_ascii_digit()) {
            continue;
        }
        return true;
    }
    false
}

fn quote_service_for_kind(kind: SymbolKind) -> SchwabService {
    match kind {
        SymbolKind::Equity => SchwabService::LevelOneEquities,
        SymbolKind::Option => SchwabService::LevelOneOptions,
        SymbolKind::Future => SchwabService::LevelOneFutures,
        SymbolKind::FutureOption => SchwabService::LevelOneFuturesOptions,
        SymbolKind::Forex => SchwabService::LevelOneForex,
    }
}

fn chart_service_for_kind(kind: SymbolKind) -> Option<SchwabService> {
    match kind {
        SymbolKind::Equity => Some(SchwabService::ChartEquity),
        SymbolKind::Future => Some(SchwabService::ChartFutures),
        SymbolKind::Option | SymbolKind::FutureOption | SymbolKind::Forex => None,
    }
}

fn book_services_for_kind(kind: SymbolKind) -> &'static [SchwabService] {
    match kind {
        SymbolKind::Equity => &[SchwabService::NyseBook, SchwabService::NasdaqBook],
        SymbolKind::Option => &[SchwabService::OptionsBook],
        SymbolKind::Future | SymbolKind::FutureOption | SymbolKind::Forex => &[],
    }
}

#[derive(Debug, Default, Clone)]
pub struct SubscriptionPlan {
    pub quote_by_service: BTreeMap<SchwabService, Vec<String>>,
    pub chart_by_service: BTreeMap<SchwabService, Vec<String>>,
    pub orderbook_by_service: BTreeMap<SchwabService, Vec<String>>,
    pub orderbook_level1_fallback: Vec<String>,
}

impl SubscriptionPlan {
    pub fn from_symbols(symbols: &[String]) -> Self {
        let mut plan = Self::default();
        for symbol in symbols {
            let kind = infer_symbol_kind(symbol);
            plan.quote_by_service
                .entry(quote_service_for_kind(kind))
                .or_default()
                .push(symbol.clone());
            if let Some(service) = chart_service_for_kind(kind) {
                plan.chart_by_service
                    .entry(service)
                    .or_default()
                    .push(symbol.clone());
            }

            let book_services = book_services_for_kind(kind);
            if book_services.is_empty() {
                plan.orderbook_level1_fallback.push(symbol.clone());
            } else {
                for service in book_services {
                    plan.orderbook_by_service
                        .entry(*service)
                        .or_default()
                        .push(symbol.clone());
                }
            }
        }
        plan
    }
}

#[derive(Debug, Clone)]
struct StreamerContext {
    socket_url: String,
    customer_id: String,
    correl_id: String,
    channel: String,
    function_id: String,
    access_token: String,
    token_obtained_at_epoch_secs: u64,
}

#[derive(Debug)]
enum ControlMessage {
    Resubscribe(SubscriptionPlan),
    Stop,
}

struct LiveRuntime {
    ctrl_tx: Sender<ControlMessage>,
    event_rx: Receiver<MarketEvent>,
    join: JoinHandle<()>,
    stop_flag: Arc<AtomicBool>,
}

impl LiveRuntime {
    fn stop(self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        let _ = self.ctrl_tx.send(ControlMessage::Stop);
        let _ = self.join.join();
    }
}

pub struct SchwabProvider {
    sim_interval: Duration,
    sim: SyntheticProvider,
    plan: SubscriptionPlan,
    symbols: Vec<String>,
    prefetch_events: Vec<MarketEvent>,
    live: Option<LiveRuntime>,
    live_mode: bool,
}

impl SchwabProvider {
    pub fn new(interval: Duration) -> Self {
        let live_mode = std::env::var("CHART_TUI_SCHWAB_MODE")
            .map(|v| v.eq_ignore_ascii_case("live"))
            .unwrap_or(true);
        Self {
            sim_interval: interval,
            sim: SyntheticProvider::new(interval),
            plan: SubscriptionPlan::default(),
            symbols: Vec::new(),
            prefetch_events: Vec::new(),
            live: None,
            live_mode,
        }
    }

    pub fn new_simulated(interval: Duration) -> Self {
        let mut p = Self::new(interval);
        p.live_mode = false;
        p
    }

    fn ensure_live_runtime(&mut self) {
        if !self.live_mode {
            return;
        }
        if self.symbols.is_empty() {
            return;
        }

        if let Some(existing) = self.live.take() {
            existing.stop();
        }

        let (ctrl_tx, ctrl_rx) = unbounded::<ControlMessage>();
        let (event_tx, event_rx) = unbounded::<MarketEvent>();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop_flag);
        let plan = self.plan.clone();

        let join = thread::spawn(move || {
            run_live_worker(plan, ctrl_rx, event_tx, stop_for_thread);
        });

        self.live = Some(LiveRuntime {
            ctrl_tx,
            event_rx,
            join,
            stop_flag,
        });
    }

    pub fn subscription_plan(&self) -> &SubscriptionPlan {
        &self.plan
    }
}

impl Drop for SchwabProvider {
    fn drop(&mut self) {
        if let Some(live) = self.live.take() {
            live.stop();
        }
    }
}

impl MarketDataProvider for SchwabProvider {
    fn name(&self) -> &'static str {
        if self.live_mode {
            "schwab-live"
        } else {
            "schwab-simulated"
        }
    }

    fn subscribe(&mut self, symbols: Vec<String>) {
        if self.symbols == symbols {
            return;
        }
        let prefetch_symbols = symbols_requiring_prefetch(&self.symbols, &symbols);
        self.symbols = symbols.clone();
        self.plan = SubscriptionPlan::from_symbols(&symbols);

        if self.live_mode {
            self.prefetch_events =
                fetch_rth_backfill_events(&prefetch_symbols).unwrap_or_else(|err| {
                    eprintln!("schwab backfill failed: {}", err);
                    Vec::new()
                });
            if self.live.is_none() {
                self.ensure_live_runtime();
            } else if let Some(runtime) = self.live.as_ref() {
                let _ = runtime
                    .ctrl_tx
                    .send(ControlMessage::Resubscribe(self.plan.clone()));
            }
        } else {
            self.sim.subscribe(symbols);
        }
    }

    fn poll_events(&mut self) -> Vec<MarketEvent> {
        if !self.live_mode {
            return self.sim.poll_events();
        }

        let mut out = Vec::new();

        if !self.prefetch_events.is_empty() {
            let take = self.prefetch_events.len().min(400);
            out.extend(self.prefetch_events.drain(..take));
        }
        let Some(runtime) = self.live.as_ref() else {
            return out;
        };

        while let Ok(event) = runtime.event_rx.try_recv() {
            out.push(event);
        }
        out
    }
}

fn symbols_requiring_prefetch(previous: &[String], next: &[String]) -> Vec<String> {
    let mut prior_counts: HashMap<String, usize> = HashMap::new();
    for sym in previous {
        *prior_counts.entry(sym.clone()).or_insert(0) += 1;
    }

    let mut out = Vec::new();
    for sym in next {
        match prior_counts.get_mut(sym) {
            Some(count) if *count > 0 => *count -= 1,
            _ => out.push(sym.clone()),
        }
    }
    out
}

fn run_live_worker(
    mut plan: SubscriptionPlan,
    ctrl_rx: Receiver<ControlMessage>,
    event_tx: Sender<MarketEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut symbol_state = HashMap::<String, SymbolState>::new();

    while !stop.load(Ordering::Relaxed) {
        match connect_live_session() {
            Ok((mut ws, mut ctx)) => {
                if let Err(err) = send_login(&mut ws, &ctx) {
                    eprintln!("schwab live login send failed: {}", err);
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }

                let mut request_id: u64 = 2;
                let mut logged_in = false;
                let mut subscribed = false;
                let mut last_refresh_attempt_epoch_secs: u64 = 0;

                while !stop.load(Ordering::Relaxed) {
                    while let Ok(cmd) = ctrl_rx.try_recv() {
                        match cmd {
                            ControlMessage::Stop => return,
                            ControlMessage::Resubscribe(new_plan) => {
                                plan = new_plan;
                                if logged_in {
                                    if let Err(err) = send_plan_subscriptions(
                                        &mut ws,
                                        &ctx,
                                        &plan,
                                        &mut request_id,
                                    ) {
                                        eprintln!("schwab live resubscribe failed: {}", err);
                                    }
                                }
                            }
                        }
                    }

                    match ws.read() {
                        Ok(msg) => {
                            if msg.is_ping() {
                                let _ = ws.send(Message::Pong(Vec::new()));
                                continue;
                            }
                            if msg.is_text() {
                                let text = match msg.into_text() {
                                    Ok(t) => t,
                                    Err(_) => continue,
                                };
                                if let Some(login_ok) = handle_response_login(&text) {
                                    if login_ok {
                                        logged_in = true;
                                    } else {
                                        eprintln!(
                                            "schwab login denied; refresh token and reconnecting"
                                        );
                                        break;
                                    }
                                }
                                if logged_in && !subscribed {
                                    if let Err(err) = send_plan_subscriptions(
                                        &mut ws,
                                        &ctx,
                                        &plan,
                                        &mut request_id,
                                    ) {
                                        eprintln!("schwab live subscribe failed: {}", err);
                                        break;
                                    }
                                    subscribed = true;
                                }

                                for event in parse_market_events(&text, &mut symbol_state) {
                                    let _ = event_tx.send(event);
                                }
                            }
                        }
                        Err(WsError::Io(e))
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            thread::sleep(IO_POLL_SLEEP);
                        }
                        Err(WsError::ConnectionClosed) | Err(WsError::AlreadyClosed) => {
                            break;
                        }
                        Err(err) => {
                            eprintln!("schwab websocket read error: {}", err);
                            break;
                        }
                    }

                    if auto_refresh_enabled()
                        && should_refresh_access_token(ctx.token_obtained_at_epoch_secs)
                    {
                        let now = now_epoch_secs();
                        if now.saturating_sub(last_refresh_attempt_epoch_secs)
                            >= REFRESH_RETRY_THROTTLE_SECONDS
                        {
                            last_refresh_attempt_epoch_secs = now;
                            match refresh_tokens_from_file() {
                                Ok(updated) => {
                                    ctx.access_token = updated.access_token;
                                    ctx.token_obtained_at_epoch_secs =
                                        updated.obtained_at_epoch_secs;
                                    eprintln!(
                                        "schwab access token auto-refreshed; reconnecting stream"
                                    );
                                    break;
                                }
                                Err(err) => {
                                    eprintln!("schwab auto-refresh failed: {}", err);
                                }
                            }
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("schwab live connect failed: {}", err);
                thread::sleep(RECONNECT_DELAY);
            }
        }
    }
}

fn refresh_tokens_from_file() -> Result<StoredTokens, String> {
    let token_path = default_token_file_path();
    let current = load_tokens(&token_path)?;
    let refresh_token = current
        .refresh_token
        .as_deref()
        .ok_or_else(|| "no refresh_token found in stored token file".to_string())?;

    let cfg = SchwabOAuthConfig::from_env()?;
    let oauth = SchwabOAuth::new(cfg);
    let token = oauth.refresh_access_token(refresh_token)?;
    let mut updated = StoredTokens::from_response(token);
    if updated.refresh_token.is_none() {
        updated.refresh_token = current.refresh_token;
    }
    save_tokens(&token_path, &updated)?;
    Ok(updated)
}

fn should_refresh_access_token(obtained_at_epoch_secs: u64) -> bool {
    if obtained_at_epoch_secs == 0 {
        return false;
    }
    now_epoch_secs().saturating_sub(obtained_at_epoch_secs) >= ACCESS_TOKEN_REFRESH_SECONDS
}

fn auto_refresh_enabled() -> bool {
    std::env::var("CHART_TUI_SCHWAB_AUTO_REFRESH")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        })
        .unwrap_or(true)
}

type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect_live_session() -> Result<(WsSocket, StreamerContext), String> {
    let token_path = default_token_file_path();
    let tokens = load_tokens(&token_path)?;
    let ctx = fetch_streamer_context(&tokens.access_token, tokens.obtained_at_epoch_secs)?;
    let (mut ws, _) = connect(ctx.socket_url.as_str()).map_err(|e| e.to_string())?;
    set_ws_nonblocking(&mut ws)?;
    Ok((ws, ctx))
}

fn set_ws_nonblocking(ws: &mut WsSocket) -> Result<(), String> {
    match ws.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_nonblocking(true).map_err(|e| e.to_string()),
        MaybeTlsStream::Rustls(stream) => stream
            .get_mut()
            .set_nonblocking(true)
            .map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

fn fetch_streamer_context(
    access_token: &str,
    obtained_at_epoch_secs: u64,
) -> Result<StreamerContext, String> {
    let client = Client::new();
    let response = client
        .get(USER_PREFERENCE_URL)
        .header(AUTHORIZATION, format!("Bearer {}", access_token))
        .header(ACCEPT, "application/json")
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "userPreference failed with status {}: {}",
            status, body
        ));
    }

    let root: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let info = root
        .get("streamerInfo")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .ok_or_else(|| "userPreference missing streamerInfo[0]".to_string())?;

    let socket_url = get_str_any(info, &["streamerSocketUrl", "streamerSocketURL"])
        .ok_or_else(|| "streamerInfo missing streamerSocketUrl".to_string())?;
    let customer_id = get_str_any(info, &["schwabClientCustomerId"])
        .ok_or_else(|| "streamerInfo missing schwabClientCustomerId".to_string())?;
    let correl_id = get_str_any(info, &["schwabClientCorrelId"])
        .ok_or_else(|| "streamerInfo missing schwabClientCorrelId".to_string())?;
    let channel = get_str_any(info, &["schwabClientChannel"]).unwrap_or_else(|| "N9".to_string());
    let function_id =
        get_str_any(info, &["schwabClientFunctionId"]).unwrap_or_else(|| "APIAPP".to_string());

    let socket_url = if socket_url.starts_with("ws://") || socket_url.starts_with("wss://") {
        socket_url
    } else {
        format!("wss://{}", socket_url)
    };

    Ok(StreamerContext {
        socket_url,
        customer_id,
        correl_id,
        channel,
        function_id,
        access_token: access_token.to_string(),
        token_obtained_at_epoch_secs: obtained_at_epoch_secs,
    })
}

#[derive(Debug, Deserialize)]
struct PriceHistoryResponse {
    #[serde(default)]
    candles: Vec<PriceHistoryCandle>,
    #[allow(dead_code)]
    empty: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PriceHistoryCandle {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    datetime: i64,
}

fn fetch_rth_backfill_events(symbols: &[String]) -> Result<Vec<MarketEvent>, String> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    let token_path = default_token_file_path();
    let tokens = load_tokens(&token_path)?;
    let client = Client::new();
    let (start_ms, end_ms) = today_rth_window_utc_ms();

    let mut out = Vec::new();
    for symbol in symbols {
        match fetch_symbol_minute_history(&client, &tokens.access_token, symbol, start_ms, end_ms) {
            Ok(events) => out.extend(events),
            Err(err) => {
                eprintln!("schwab backfill skipped for {}: {}", symbol, err);
            }
        }
    }
    Ok(out)
}

fn fetch_symbol_minute_history(
    client: &Client,
    access_token: &str,
    symbol: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<Vec<MarketEvent>, String> {
    let params = vec![
        ("symbol".to_string(), symbol.to_string()),
        ("periodType".to_string(), "day".to_string()),
        ("period".to_string(), "1".to_string()),
        ("frequencyType".to_string(), "minute".to_string()),
        ("frequency".to_string(), "1".to_string()),
        ("needExtendedHoursData".to_string(), "false".to_string()),
        ("needPreviousClose".to_string(), "false".to_string()),
        ("startDate".to_string(), start_ms.to_string()),
        ("endDate".to_string(), end_ms.to_string()),
    ];

    let response = client
        .get(PRICE_HISTORY_URL)
        .header(AUTHORIZATION, format!("Bearer {}", access_token))
        .header(ACCEPT, "application/json")
        .header(ACCEPT_ENCODING, "identity")
        .query(&params)
        .send()
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let encoding = response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase());
    let body_bytes = response.bytes().map_err(|e| e.to_string())?;
    let body = decode_http_body(&body_bytes, encoding.as_deref());
    if !status.is_success() {
        return Err(format!(
            "history request failed with status {}: {}",
            status, body
        ));
    }

    let mut parsed: PriceHistoryResponse =
        serde_json::from_str(&body).map_err(|e| format!("invalid history json: {}", e))?;
    normalize_history_volumes(&mut parsed.candles);

    let mut out = Vec::with_capacity(parsed.candles.len());
    for c in parsed.candles {
        let ts = ((c.datetime.max(0) as u64) / 60_000).max(1);
        let candle = Candle {
            ts,
            open: c.open.max(0.01),
            high: c.high.max(c.open.max(c.close)).max(0.01),
            low: c.low.min(c.open.min(c.close)).max(0.01),
            close: c.close.max(0.01),
            volume: c.volume.max(0.0),
        };
        out.push(MarketEvent {
            symbol: symbol.to_string(),
            orderbook: OrderBookSnapshot::empty(candle.close),
            candle,
        });
    }
    Ok(out)
}

fn normalize_history_volumes(candles: &mut [PriceHistoryCandle]) {
    if candles.len() < 2 {
        return;
    }

    let monotonic_steps = candles
        .windows(2)
        .filter(|w| w[1].volume >= w[0].volume)
        .count();
    let ratio = monotonic_steps as f64 / (candles.len().saturating_sub(1)) as f64;
    if ratio < 0.95 {
        return;
    }

    let mut prev = 0.0_f64;
    for c in candles {
        let current = c.volume.max(0.0);
        let delta = if current >= prev {
            current - prev
        } else {
            current
        };
        c.volume = delta.max(0.0);
        prev = current;
    }
}

fn today_rth_window_utc_ms() -> (u64, u64) {
    let now_utc = Utc::now();
    let now_et = now_utc.with_timezone(&New_York);
    let date = now_et.date_naive();
    let open_et = New_York
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 9, 30, 0)
        .single()
        .unwrap_or(now_et);

    let start = open_et.with_timezone(&Utc).timestamp_millis().max(0) as u64;
    let end = now_utc.timestamp_millis().max(0) as u64;
    (start, end)
}

fn send_login(ws: &mut WsSocket, ctx: &StreamerContext) -> Result<(), String> {
    let payload = json!({
        "requests": [
            {
                "service": "ADMIN",
                "requestid": "1",
                "command": "LOGIN",
                "SchwabClientCustomerId": ctx.customer_id,
                "SchwabClientCorrelId": ctx.correl_id,
                "parameters": {
                    "Authorization": ctx.access_token,
                    "SchwabClientChannel": ctx.channel,
                    "SchwabClientFunctionId": ctx.function_id,
                }
            }
        ]
    });
    ws.send(Message::Text(payload.to_string()))
        .map_err(|e| e.to_string())
}

fn send_plan_subscriptions(
    ws: &mut WsSocket,
    ctx: &StreamerContext,
    plan: &SubscriptionPlan,
    request_id: &mut u64,
) -> Result<(), String> {
    let mut requests = Vec::new();

    for (service, symbols) in &plan.quote_by_service {
        if symbols.is_empty() {
            continue;
        }
        requests.push(subscription_request(
            service.as_str(),
            *request_id,
            ctx,
            symbols,
            service.default_fields(),
        ));
        *request_id += 1;
    }

    for (service, symbols) in &plan.chart_by_service {
        if symbols.is_empty() {
            continue;
        }
        requests.push(subscription_request(
            service.as_str(),
            *request_id,
            ctx,
            symbols,
            service.default_fields(),
        ));
        *request_id += 1;
    }

    for (service, symbols) in &plan.orderbook_by_service {
        if symbols.is_empty() {
            continue;
        }
        requests.push(subscription_request(
            service.as_str(),
            *request_id,
            ctx,
            symbols,
            service.default_fields(),
        ));
        *request_id += 1;
    }

    if requests.is_empty() {
        return Ok(());
    }

    let payload = json!({ "requests": requests });
    ws.send(Message::Text(payload.to_string()))
        .map_err(|e| e.to_string())
}

fn subscription_request(
    service: &str,
    request_id: u64,
    ctx: &StreamerContext,
    symbols: &[String],
    fields: &str,
) -> Value {
    json!({
        "service": service,
        "requestid": request_id.to_string(),
        "command": "SUBS",
        "SchwabClientCustomerId": ctx.customer_id,
        "SchwabClientCorrelId": ctx.correl_id,
        "parameters": {
            "keys": symbols.join(","),
            "fields": fields,
        }
    })
}

fn handle_response_login(text: &str) -> Option<bool> {
    let root: Value = serde_json::from_str(text).ok()?;
    let responses = root.get("response")?.as_array()?;
    for resp in responses {
        if resp.get("service").and_then(Value::as_str) != Some("ADMIN") {
            continue;
        }
        if resp.get("command").and_then(Value::as_str) != Some("LOGIN") {
            continue;
        }
        let code = resp
            .get("content")
            .and_then(|c| c.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        return Some(code == 0);
    }
    None
}

#[derive(Debug, Default, Clone)]
struct QuoteSnapshot {
    bid: Option<f64>,
    ask: Option<f64>,
    last: Option<f64>,
    bid_size: Option<f64>,
    ask_size: Option<f64>,
    total_volume: Option<f64>,
}

#[derive(Debug, Default, Clone)]
struct SymbolState {
    quote: QuoteSnapshot,
    candle: Option<Candle>,
    orderbook: Option<OrderBookSnapshot>,
    candle_minute: Option<u64>,
    last_total_volume: Option<f64>,
    last_chart_update_ms: Option<u64>,
}

fn parse_market_events(
    text: &str,
    symbol_state: &mut HashMap<String, SymbolState>,
) -> Vec<MarketEvent> {
    let root: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    let Some(data) = root.get("data").and_then(Value::as_array) else {
        return out;
    };

    for packet in data {
        let Some(service) = packet.get("service").and_then(Value::as_str) else {
            continue;
        };
        let Some(content) = packet.get("content").and_then(Value::as_array) else {
            continue;
        };

        for entry in content {
            match service {
                "CHART_EQUITY" | "CHART_FUTURES" => {
                    if let Some((symbol, candle)) = parse_chart_entry(service, entry) {
                        let state = symbol_state.entry(symbol.clone()).or_default();
                        state.quote.last = Some(candle.close);
                        state.candle = Some(candle);
                        state.last_chart_update_ms = Some(now_epoch_ms());
                        emit_if_ready(&symbol, state, &mut out);
                    }
                }
                "NYSE_BOOK" | "NASDAQ_BOOK" | "OPTIONS_BOOK" => {
                    if let Some((symbol, book)) = parse_book_entry(entry) {
                        let state = symbol_state.entry(symbol.clone()).or_default();
                        state.orderbook = Some(book);
                        emit_if_ready(&symbol, state, &mut out);
                    }
                }
                "LEVELONE_EQUITIES"
                | "LEVELONE_OPTIONS"
                | "LEVELONE_FUTURES"
                | "LEVELONE_FUTURES_OPTIONS"
                | "LEVELONE_FOREX" => {
                    if let Some(symbol) = parse_symbol(entry) {
                        let state = symbol_state.entry(symbol.clone()).or_default();
                        apply_quote_update(state, entry);
                        if should_build_quote_candle(service, state.last_chart_update_ms) {
                            update_quote_candle(state);
                        }
                        let should_refresh_fallback_book = matches!(
                            service,
                            "LEVELONE_FUTURES" | "LEVELONE_FUTURES_OPTIONS" | "LEVELONE_FOREX"
                        );
                        if should_refresh_fallback_book {
                            state.orderbook = fallback_orderbook(&state.quote);
                        } else if state.orderbook.is_none() {
                            state.orderbook = fallback_orderbook(&state.quote);
                        }
                        emit_if_ready(&symbol, state, &mut out);
                    }
                }
                _ => {}
            }
        }
    }

    out
}

fn parse_symbol(entry: &Value) -> Option<String> {
    get_str_any(entry, &["key", "0"]).map(|s| s.trim().to_ascii_uppercase())
}

fn parse_chart_entry(service: &str, entry: &Value) -> Option<(String, Candle)> {
    let symbol = parse_symbol(entry)?;
    let (ts_ms, open, high, low, close, volume) = if service == "CHART_EQUITY" {
        (
            get_f64_any(entry, &["7"])?,
            get_f64_any(entry, &["1"])?,
            get_f64_any(entry, &["2"])?,
            get_f64_any(entry, &["3"])?,
            get_f64_any(entry, &["4"])?,
            get_f64_any(entry, &["5"]).unwrap_or(0.0),
        )
    } else {
        (
            get_f64_any(entry, &["1"])?,
            get_f64_any(entry, &["2"])?,
            get_f64_any(entry, &["3"])?,
            get_f64_any(entry, &["4"])?,
            get_f64_any(entry, &["5"])?,
            get_f64_any(entry, &["6"]).unwrap_or(0.0),
        )
    };

    let minute = (ts_ms.max(0.0) as u64) / 60_000;
    Some((
        symbol,
        Candle {
            ts: minute,
            open,
            high: high.max(open.max(close)),
            low: low.min(open.min(close)),
            close,
            volume,
        },
    ))
}

fn parse_book_entry(entry: &Value) -> Option<(String, OrderBookSnapshot)> {
    let symbol = parse_symbol(entry)?;
    let bids = parse_price_levels(entry.get("2"), true);
    let asks = parse_price_levels(entry.get("3"), false);

    if bids.is_empty() && asks.is_empty() {
        return None;
    }

    let best_bid = bids.first().map(|l| l.price).unwrap_or(0.0);
    let best_ask = asks.first().map(|l| l.price).unwrap_or(0.0);
    let mid = if best_bid > 0.0 && best_ask > 0.0 {
        (best_bid + best_ask) * 0.5
    } else {
        best_bid.max(best_ask)
    };

    Some((
        symbol,
        OrderBookSnapshot {
            mid_price: mid.max(0.01),
            bids,
            asks,
        },
    ))
}

fn parse_price_levels(value: Option<&Value>, bids: bool) -> Vec<OrderBookLevel> {
    let mut out = Vec::new();
    let Some(levels) = value.and_then(Value::as_array) else {
        return out;
    };

    for level in levels {
        let (price, size) = if let Some(arr) = level.as_array() {
            let p = arr.first().and_then(as_f64_value).unwrap_or(0.0);
            let s = arr.get(1).and_then(as_f64_value).unwrap_or(0.0);
            (p, s)
        } else {
            (
                get_f64_any(level, &["0"]).unwrap_or(0.0),
                get_f64_any(level, &["1"]).unwrap_or(0.0),
            )
        };

        if price > 0.0 {
            out.push(OrderBookLevel {
                price,
                size: size.max(0.1),
            });
        }
    }

    if bids {
        out.sort_by(|a, b| {
            b.price
                .partial_cmp(&a.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        out.sort_by(|a, b| {
            a.price
                .partial_cmp(&b.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    out.truncate(BOOK_DEPTH);
    out
}

fn apply_quote_update(state: &mut SymbolState, entry: &Value) {
    if let Some(v) = get_f64_any(entry, &["1", "bidPrice"]) {
        state.quote.bid = Some(v);
    }
    if let Some(v) = get_f64_any(entry, &["2", "askPrice"]) {
        state.quote.ask = Some(v);
    }
    if let Some(v) = get_f64_any(entry, &["3", "lastPrice", "mark"]) {
        state.quote.last = Some(v);
    }
    if let Some(v) = get_f64_any(entry, &["4", "bidSize", "16"]) {
        state.quote.bid_size = Some(v);
    }
    if let Some(v) = get_f64_any(entry, &["5", "askSize", "17"]) {
        state.quote.ask_size = Some(v);
    }
    if let Some(v) = get_f64_any(entry, &["8", "totalVolume", "volume"]) {
        state.quote.total_volume = Some(v);
    }
}

fn update_quote_candle(state: &mut SymbolState) {
    let price = match state
        .quote
        .last
        .or_else(|| match (state.quote.bid, state.quote.ask) {
            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => Some((b + a) * 0.5),
            (Some(b), _) => Some(b),
            (_, Some(a)) => Some(a),
            _ => None,
        }) {
        Some(v) if v > 0.0 => v,
        _ => return,
    };

    let minute = now_epoch_ms() / 60_000;
    let current_total = state.quote.total_volume;
    let delta = match (current_total, state.last_total_volume) {
        (Some(now), Some(prev)) if now >= prev => (now - prev).max(0.0),
        (Some(now), None) => now.max(0.0),
        _ => 0.0,
    };
    state.last_total_volume = current_total;

    if state.candle_minute != Some(minute) {
        state.candle_minute = Some(minute);
        state.candle = Some(Candle {
            ts: minute,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: delta,
        });
        return;
    }

    if let Some(c) = state.candle.as_mut() {
        c.close = price;
        c.high = c.high.max(price);
        c.low = c.low.min(price);
        c.volume += delta;
    } else {
        state.candle = Some(Candle {
            ts: minute,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: delta,
        });
    }
}

fn should_build_quote_candle(service: &str, last_chart_update_ms: Option<u64>) -> bool {
    if matches!(service, "LEVELONE_EQUITIES") {
        return false;
    }

    let chart_service_expected = matches!(service, "LEVELONE_FUTURES");
    if !chart_service_expected {
        return true;
    }
    let Some(ts) = last_chart_update_ms else {
        return true;
    };
    now_epoch_ms().saturating_sub(ts) > 120_000
}

fn fallback_orderbook(quote: &QuoteSnapshot) -> Option<OrderBookSnapshot> {
    let bid = quote.bid?;
    let ask = quote.ask?;
    if bid <= 0.0 || ask <= 0.0 {
        return None;
    }

    let best_bid_size = quote.bid_size.unwrap_or(1.0).max(0.1);
    let best_ask_size = quote.ask_size.unwrap_or(1.0).max(0.1);
    let mid = ((bid + ask) * 0.5).max(0.01);
    let spread = (ask - bid).max(mid * 0.00005);
    let tick = (spread * 0.5).max(mid * 0.00002);

    let mut bids = Vec::with_capacity(BOOK_DEPTH);
    let mut asks = Vec::with_capacity(BOOK_DEPTH);
    for level in 0..BOOK_DEPTH {
        let step = level as f64;
        let decay = 1.0 / (1.0 + step * 0.35);

        bids.push(OrderBookLevel {
            price: (bid - step * tick).max(0.01),
            size: (best_bid_size * decay).round().max(1.0),
        });
        asks.push(OrderBookLevel {
            price: (ask + step * tick).max(0.01),
            size: (best_ask_size * decay).round().max(1.0),
        });
    }

    Some(OrderBookSnapshot {
        mid_price: mid,
        bids,
        asks,
    })
}

fn emit_if_ready(symbol: &str, state: &mut SymbolState, out: &mut Vec<MarketEvent>) {
    let Some(candle) = state.candle else {
        return;
    };

    let orderbook = state
        .orderbook
        .clone()
        .or_else(|| fallback_orderbook(&state.quote))
        .unwrap_or_else(|| OrderBookSnapshot::empty(candle.close));

    out.push(MarketEvent {
        symbol: symbol.to_string(),
        candle,
        orderbook,
    });
}

fn get_str_any(obj: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if v.is_number() || v.is_boolean() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn get_f64_any(obj: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = as_f64_value(v) {
                return Some(n);
            }
        }
    }
    None
}

fn as_f64_value(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(i) = v.as_i64() {
        return Some(i as f64);
    }
    if let Some(u) = v.as_u64() {
        return Some(u as f64);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn decode_http_body(body: &[u8], encoding: Option<&str>) -> String {
    let is_gzip = encoding
        .map(|e| e.contains("gzip"))
        .unwrap_or_else(|| body.starts_with(&[0x1f, 0x8b]));
    if is_gzip {
        let mut decoder = GzDecoder::new(body);
        let mut decoded = String::new();
        if decoder.read_to_string(&mut decoded).is_ok() {
            return decoded;
        }
    }
    String::from_utf8_lossy(body).to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        infer_symbol_kind, parse_book_entry, parse_chart_entry, parse_market_events, SchwabService,
        SubscriptionPlan, SymbolKind,
    };
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn infers_symbol_kinds() {
        assert_eq!(infer_symbol_kind("AAPL"), SymbolKind::Equity);
        assert_eq!(infer_symbol_kind("/ES"), SymbolKind::Future);
        assert_eq!(infer_symbol_kind("/ESH26"), SymbolKind::Future);
        assert_eq!(
            infer_symbol_kind("AAPL  251219C00200000"),
            SymbolKind::Option
        );
        assert_eq!(infer_symbol_kind("EUR/USD"), SymbolKind::Forex);
    }

    #[test]
    fn builds_service_bucket_plan() {
        let plan = SubscriptionPlan::from_symbols(&[
            "AAPL".to_string(),
            "/ES".to_string(),
            "AAPL  251219C00200000".to_string(),
        ]);
        assert_eq!(
            plan.quote_by_service.get(&SchwabService::LevelOneEquities),
            Some(&vec!["AAPL".to_string()])
        );
        assert_eq!(
            plan.quote_by_service.get(&SchwabService::LevelOneFutures),
            Some(&vec!["/ES".to_string()])
        );
        assert_eq!(
            plan.quote_by_service.get(&SchwabService::LevelOneOptions),
            Some(&vec!["AAPL  251219C00200000".to_string()])
        );

        assert_eq!(
            plan.chart_by_service.get(&SchwabService::ChartEquity),
            Some(&vec!["AAPL".to_string()])
        );
        assert_eq!(
            plan.chart_by_service.get(&SchwabService::ChartFutures),
            Some(&vec!["/ES".to_string()])
        );

        assert_eq!(
            plan.orderbook_by_service.get(&SchwabService::NyseBook),
            Some(&vec!["AAPL".to_string()])
        );
        assert_eq!(
            plan.orderbook_by_service.get(&SchwabService::NasdaqBook),
            Some(&vec!["AAPL".to_string()])
        );
        assert_eq!(
            plan.orderbook_by_service.get(&SchwabService::OptionsBook),
            Some(&vec!["AAPL  251219C00200000".to_string()])
        );
        assert_eq!(plan.orderbook_level1_fallback, vec!["/ES".to_string()]);
    }

    #[test]
    fn parses_chart_entries() {
        let eq = json!({"0":"AAPL","1":10.0,"2":11.0,"3":9.5,"4":10.5,"5":1000.0,"7":120000});
        let (sym, candle) = parse_chart_entry("CHART_EQUITY", &eq).expect("chart equity");
        assert_eq!(sym, "AAPL");
        assert_eq!(candle.ts, 2);
        assert_eq!(candle.close, 10.5);

        let fut =
            json!({"0":"/ES","1":180000,"2":5100.0,"3":5110.0,"4":5090.0,"5":5105.0,"6":2000.0});
        let (sym_f, candle_f) = parse_chart_entry("CHART_FUTURES", &fut).expect("chart futures");
        assert_eq!(sym_f, "/ES");
        assert_eq!(candle_f.ts, 3);
        assert_eq!(candle_f.volume, 2000.0);
    }

    #[test]
    fn parses_book_entry_with_arrays() {
        let raw = json!({
            "key":"AAPL",
            "2":[[100.0,20],[99.5,10]],
            "3":[[100.5,25],[101.0,11]]
        });
        let (_, ob) = parse_book_entry(&raw).expect("book parse");
        assert_eq!(ob.bids[0].price, 100.0);
        assert_eq!(ob.asks[0].price, 100.5);
        assert!(ob.mid_price > 100.0);
    }

    #[test]
    fn parses_levelone_to_market_event() {
        let msg = json!({
            "data":[
                {
                    "service":"LEVELONE_EQUITIES",
                    "content":[
                        {"key":"AAPL","1":100.0,"2":101.0,"3":100.5,"4":10,"5":11,"8":5000}
                    ]
                }
            ]
        })
        .to_string();

        let mut state = HashMap::new();
        let events = parse_market_events(&msg, &mut state);
        assert!(!events.is_empty());
        assert_eq!(events[0].symbol, "AAPL");
        assert_eq!(events[0].orderbook.bids.len(), super::BOOK_DEPTH);
        assert_eq!(events[0].orderbook.asks.len(), super::BOOK_DEPTH);
    }
}
