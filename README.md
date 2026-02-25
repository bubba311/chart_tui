# chart_tui

High-performance Rust TUI for multi-chart candlestick visualization with log-scale price axis, layout switching, timeframe aggregation, and live synthetic market data.

## Features
- Provider abstraction:
  - `MarketDataProvider` trait for pluggable feeds
  - `SyntheticProvider` implementation
  - `SchwabProvider` as the default runtime provider (live WebSocket mode)
  - Schwab OAuth scaffolding module for authorization URL/token endpoint wiring
- Multi-layout charting:
  - Single chart
  - Two-chart split
  - 2x2 quad grid
- Timeframes:
  - `1m`, `3m`, `15m`, `45m`, `1h`, `3h`, `4h`, `1d`
- Default view:
  - Startup opens at `1m`
  - Ticker switch resets active pane to `1m`
- Timeframe behavior:
  - Higher timeframe candles update in place until the bucket closes
- Always-log vertical axis:
  - Log price mapping for candle positioning
  - Rounded, human-friendly axis ticks
- Candlestick rendering:
  - Colored solid bullish/bearish bodies
  - Wick rendering
  - Last-price marker line and label
- Single-layout orderbook popout:
  - Toggleable right-side orderbook panel
  - Synthetic bid/ask ladders centered on current candle close
- Performance-focused runtime:
  - Fixed timestep frame pacing
  - Dirty-pane range recomputation
  - Runtime metrics footer (`FPS`, frame/update/render timings, feed events)
- Stochastic synthetic data feed:
  - Regime shifts, volatility changes, spikes, gaps, and volume bursts
  - Synthetic orderbook with dynamic spread/depth tied to volatility and volume

## Controls
- `q`: quit
- `Tab`: next active pane
- `1` / `2` / `4`: switch layout
- `o`: toggle orderbook panel (single layout)
- `,` / `.`: previous/next ticker (active pane)
- `t`: start ticker input mode
- `A-Z`/`0-9` plus `/ . - _ :` + `Enter`: submit ticker for active pane (from symbol universe)
- `Esc`: cancel ticker input mode
- `[` / `]`: previous/next timeframe (active pane)
- `Left` / `Right`: pan (active pane)
- `Up` / `Down`: zoom (active pane)
- `+` / `-`: zoom (active pane)

## Build and Run
```bash
cd /home/ryan/playground/chart_tui
cargo run
```

## Schwab OAuth (CLI)
Generate a ready-to-use Schwab authorization URL:

```bash
cd /home/ryan/playground/chart_tui
export SCHWAB_CLIENT_ID="your_client_id"
export SCHWAB_CLIENT_SECRET="your_client_secret"
export SCHWAB_REDIRECT_URI="https://127.0.0.1"
export SCHWAB_SCOPE="readonly"

cargo run -- schwab-auth-url --state dev-state
```

Optionally write output to a file:

```bash
cargo run -- schwab-auth-url --state dev-state --out schwab_auth.txt
```

Login and persist tokens locally (recommended workflow):

```bash
cargo run -- schwab-login --state dev-state
```

You will be prompted to paste the callback URL (or just the `code` parameter).  
By default tokens are written to:
- `$CHART_TUI_SCHWAB_TOKEN_FILE` if set
- else `$XDG_CONFIG_HOME/chart_tui/schwab_tokens.json`
- else `~/.config/chart_tui/schwab_tokens.json`

Refresh access token from stored refresh token:

```bash
cargo run -- schwab-refresh-token
```

In live Schwab mode, access token auto-refresh is attempted every 29 minutes
using the stored refresh token and current `SCHWAB_CLIENT_ID`/`SCHWAB_CLIENT_SECRET`.
When refreshed, the streamer reconnects automatically.

You can copy `.env.example` as a starting point for local env setup.

## Provider Selection
Choose provider by environment variable:

```bash
export CHART_TUI_PROVIDER=synthetic
# or
export CHART_TUI_PROVIDER=schwab      # default; Schwab live WebSocket stream
export CHART_TUI_SCHWAB_MODE=live
```

Current default is `schwab` if `CHART_TUI_PROVIDER` is not set.

Current `schwab` mode uses Schwab service classification and subscription planning with live stream transport:
- Equities:
  - Quotes: `LEVELONE_EQUITIES`
  - Candles: `CHART_EQUITY`
  - Orderbook: `NYSE_BOOK` + `NASDAQ_BOOK`
- Futures:
  - Quotes: `LEVELONE_FUTURES`
  - Candles: `CHART_FUTURES`
  - Orderbook: level-1 fallback (no dedicated futures book service in current streamer spec)
- Options:
  - Quotes: `LEVELONE_OPTIONS`
  - Candles: derived from quote stream in current runtime
  - Orderbook: `OPTIONS_BOOK`
- Historical prefill:
  - On launch and symbol subscription changes, app requests 1-minute price history from today's US regular-session open (9:30 ET) to now, then continues with live stream updates.

## Validation
```bash
cargo fmt --check
cargo check
cargo test
```

## Notes
- View policy is layout-aware: charts auto-fit to layout threshold by default.
- Manual pan/zoom disables auto-fit for that pane until layout/timeframe is changed.
- Typed ticker behavior:
  - Press `t`, type symbol, press `Enter`.
  - Unknown symbols show `No data stream for <SYMBOL>` and current ticker remains unchanged.
  - Input supports stock, futures, and options symbols (e.g. `AAPL`, `/ES`, `AAPL251219C00200000`).
- In single layout, the orderbook panel always reflects the active pane ticker.

## Provider Architecture
- Provider trait: [src/providers/mod.rs](/home/ryan/playground/chart_tui/src/providers/mod.rs)
- Synthetic provider: [src/providers/synthetic.rs](/home/ryan/playground/chart_tui/src/providers/synthetic.rs)
- Schwab OAuth scaffolding: [src/providers/schwab/mod.rs](/home/ryan/playground/chart_tui/src/providers/schwab/mod.rs)

The app runtime consumes normalized provider events (`symbol + candle + orderbook`) so new APIs can be added without changing chart/orderbook rendering logic.

## Schwab OAuth Scaffolding
Set environment variables:
- `SCHWAB_CLIENT_ID`
- `SCHWAB_CLIENT_SECRET` (required for token exchange for most app configs)
- `SCHWAB_REDIRECT_URI` (must be `https://...`)
- `SCHWAB_SCOPE` (optional, defaults to `readonly`)
- `CHART_TUI_SCHWAB_TOKEN_FILE` (optional token file override)
- `CHART_TUI_SCHWAB_MODE` (`live` or `simulated`, default `live`)

Auth endpoint wired:
- `https://api.schwabapi.com/v1/oauth/authorize`

Token endpoint wired:
- `https://api.schwabapi.com/v1/oauth/token`
