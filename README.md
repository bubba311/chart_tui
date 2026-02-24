# chart_tui

High-performance Rust TUI for multi-chart candlestick visualization with log-scale price axis, layout switching, timeframe aggregation, and live synthetic market data.

## Features
- Multi-layout charting:
  - Single chart
  - Two-chart split
  - 2x2 quad grid
- Timeframes:
  - `1m`, `3m`, `15m`, `45m`, `1h`, `3h`, `4h`, `1d`
- Timeframe behavior:
  - Higher timeframe candles update in place until the bucket closes
- Always-log vertical axis:
  - Log price mapping for candle positioning
  - Rounded, human-friendly axis ticks
- Candlestick rendering:
  - Colored solid bullish/bearish bodies
  - Wick rendering
  - Last-price marker line and label
- Performance-focused runtime:
  - Fixed timestep frame pacing
  - Dirty-pane range recomputation
  - Runtime metrics footer (`FPS`, frame/update/render timings, feed events)
- Stochastic synthetic data feed:
  - Regime shifts, volatility changes, spikes, gaps, and volume bursts

## Controls
- `q`: quit
- `Tab`: next active pane
- `1` / `2` / `4`: switch layout
- `[` / `]`: previous/next timeframe (active pane)
- `Left` / `Right`: pan (active pane)
- `Up` / `Down`: zoom (active pane)
- `+` / `-`: zoom (active pane)

## Build and Run
```bash
cd /home/ryan/playground/chart_tui
cargo run
```

## Validation
```bash
cargo fmt --check
cargo check
cargo test
```

## Notes
- View policy is layout-aware: charts auto-fit to layout threshold by default.
- Manual pan/zoom disables auto-fit for that pane until layout/timeframe is changed.
