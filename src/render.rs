use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, ChartPane, LayoutMode};
use crate::perf::StatsSnapshot;

const AXIS_WIDTH: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App, stats: &StatsSnapshot) {
    let base = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(frame.area());
    let chart_area = base[0];
    let footer_area = base[1];

    let visible = app.visible_pane_indices();
    let areas = build_layout_areas(chart_area, app.layout, visible.len());

    for (slot, pane_idx) in visible.iter().enumerate() {
        if let (Some(area), Some(pane)) = (areas.get(slot), app.panes.get(*pane_idx)) {
            draw_pane(frame, *area, pane, *pane_idx == app.active_pane);
        }
    }

    let metrics = format!(
        "L:{:?} FPS:{:>5.1} F us(a/p95/p99):{}/{}/{} U:{} R:{} feed:{}",
        app.layout,
        stats.fps_estimate,
        stats.frame_avg_us,
        stats.frame_p95_us,
        stats.frame_p99_us,
        stats.update_avg_us,
        stats.render_avg_us,
        stats.last_feed_events
    );
    let metrics = fit_to_width(&metrics, footer_area.width as usize);
    let commands_1 = fit_to_width(
        "keys: q quit | Tab next pane | 1/2/4 layout | [ prev tf | ] next tf",
        footer_area.width as usize,
    );
    let commands_2 = fit_to_width(
        "      Left/Right pan | Up/Down zoom | +/- zoom",
        footer_area.width as usize,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(metrics),
            Line::from(commands_1),
            Line::from(commands_2),
        ])
        .style(Style::default().fg(Color::DarkGray)),
        footer_area,
    );
}

fn fit_to_width(input: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let count = input.chars().count();
    if count <= width {
        return input.to_string();
    }

    if width <= 3 {
        return ".".repeat(width);
    }

    let mut out = String::with_capacity(width);
    for ch in input.chars().take(width - 3) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn build_layout_areas(area: Rect, layout: LayoutMode, pane_count: usize) -> Vec<Rect> {
    if pane_count == 0 {
        return Vec::new();
    }

    match layout {
        LayoutMode::Single => vec![area],
        LayoutMode::TwoUp => {
            let row = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);

            row.iter().copied().take(pane_count.min(2)).collect()
        }
        LayoutMode::Quad => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);

            let top_row = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[0]);

            let bottom_row = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[1]);

            [top_row[0], top_row[1], bottom_row[0], bottom_row[1]]
                .into_iter()
                .take(pane_count.min(4))
                .collect()
        }
    }
}

fn draw_pane(frame: &mut Frame, area: Rect, pane: &ChartPane, is_active: bool) {
    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let last_close = pane.chart.last().map(|c| c.close).unwrap_or(0.0);
    let title = format!(
        "{} {} | candles: {} | view: {} | last: {:.2}",
        pane.symbol,
        pane.timeframe.label(),
        pane.chart.len(),
        pane.chart.visible_count(),
        last_close
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (chart_area, axis_area) = split_chart_and_axis(inner);
    draw_candles(frame.buffer_mut(), chart_area, pane);
    draw_price_axis(frame.buffer_mut(), axis_area, pane);
    draw_last_price_marker(frame.buffer_mut(), chart_area, axis_area, pane);
}

fn split_chart_and_axis(area: Rect) -> (Rect, Rect) {
    if area.width <= AXIS_WIDTH + 2 {
        return (
            area,
            Rect {
                x: area.x + area.width,
                y: area.y,
                width: 0,
                height: area.height,
            },
        );
    }

    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(AXIS_WIDTH)])
        .split(area);

    (parts[0], parts[1])
}

fn draw_candles(buf: &mut Buffer, area: Rect, pane: &ChartPane) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if pane.chart.len() == 0 {
        buf.set_string(
            area.x,
            area.y,
            "waiting for feed...",
            Style::default().fg(Color::DarkGray),
        );
        return;
    }

    if let Some((start, end)) = pane.chart.visible_indices() {
        let visible = end - start;
        for (rel, idx) in (start..end).enumerate() {
            let Some(candle) = pane.chart.get(idx) else {
                continue;
            };
            let x0 = area.x + ((rel as u32 * area.width as u32) / visible as u32) as u16;
            let next = (((rel + 1) as u32 * area.width as u32) / visible as u32) as u16;
            let mut x1 = area.x + next.saturating_sub(1).min(area.width.saturating_sub(1));
            if x1 < x0 {
                x1 = x0;
            }
            let wick_x = x0 + (x1 - x0) / 2;

            let Some(high_row) = pane.chart.map_price_to_row(candle.high, area.height) else {
                continue;
            };
            let Some(low_row) = pane.chart.map_price_to_row(candle.low, area.height) else {
                continue;
            };
            let Some(open_row) = pane.chart.map_price_to_row(candle.open, area.height) else {
                continue;
            };
            let Some(close_row) = pane.chart.map_price_to_row(candle.close, area.height) else {
                continue;
            };

            let wick_top = high_row.min(low_row);
            let wick_bottom = high_row.max(low_row);
            for row in wick_top..=wick_bottom {
                buf[(wick_x, area.y + row)]
                    .set_symbol("│")
                    .set_style(Style::default().fg(Color::Gray));
            }

            let body_top = open_row.min(close_row);
            let body_bottom = open_row.max(close_row);
            let bullish = candle.close >= candle.open;
            let body_color = if bullish { Color::Green } else { Color::Red };

            for x in x0..=x1 {
                for row in body_top..=body_bottom {
                    buf[(x, area.y + row)]
                        .set_symbol("█")
                        .set_style(Style::default().fg(body_color));
                }
            }
        }
    }
}

fn draw_price_axis(buf: &mut Buffer, area: Rect, pane: &ChartPane) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(range) = pane.chart.cached_range() else {
        return;
    };
    if range.min <= 0.0 || range.max <= 0.0 || range.max < range.min {
        return;
    }
    let ticks = rounded_ticks(range.min, range.max, usize::from(area.height.min(6)));

    for price in ticks {
        let Some(row) = pane.chart.map_price_to_row(price, area.height) else {
            continue;
        };
        let y = area.y + row;

        buf[(area.x, y)]
            .set_symbol("┤")
            .set_style(Style::default().fg(Color::DarkGray));
        let label = format_price_label(price);
        draw_clipped_text(
            buf,
            area.x + 1,
            y,
            area.width.saturating_sub(1),
            &label,
            Color::Gray,
        );
    }
}

fn draw_last_price_marker(buf: &mut Buffer, chart_area: Rect, axis_area: Rect, pane: &ChartPane) {
    let Some(last) = pane.chart.last() else {
        return;
    };
    let Some(row) = pane.chart.map_price_to_row(last.close, chart_area.height) else {
        return;
    };
    let y = chart_area.y + row;
    let color = if last.close >= last.open {
        Color::Green
    } else {
        Color::Red
    };

    for x in chart_area.x..(chart_area.x + chart_area.width) {
        buf[(x, y)]
            .set_symbol("─")
            .set_style(Style::default().fg(Color::DarkGray));
    }

    if axis_area.width > 0 {
        buf[(axis_area.x, y)]
            .set_symbol("▶")
            .set_style(Style::default().fg(color));
        let label = format_price_label(last.close);
        draw_clipped_text(
            buf,
            axis_area.x + 1,
            y,
            axis_area.width.saturating_sub(1),
            &label,
            color,
        );
    }
}

fn rounded_ticks(min: f64, max: f64, max_ticks: usize) -> Vec<f64> {
    if max_ticks == 0 || min <= 0.0 || max <= 0.0 || max < min {
        return Vec::new();
    }
    if (max - min).abs() < f64::EPSILON {
        return vec![round_to_step(min, nice_step(min / 10.0))];
    }

    let target = max_ticks.max(2);
    let step = nice_step((max - min) / (target - 1) as f64);
    let first = (min / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut v = first;
    let end = (max / step).floor() * step;
    while v <= end + step * 0.5 {
        ticks.push(round_to_step(v, step));
        v += step;
    }

    if ticks.is_empty() {
        ticks.push(round_to_step(min, step));
        ticks.push(round_to_step(max, step));
    }
    ticks
}

fn format_price_label(price: f64) -> String {
    let abs = price.abs();
    if abs >= 1_000.0 {
        format!("{:>8.0}", price)
    } else if abs >= 100.0 {
        format!("{:>8.1}", price)
    } else if abs >= 1.0 {
        format!("{:>8.2}", price)
    } else {
        format!("{:>8.4}", price)
    }
}

fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 || !raw.is_finite() {
        return 1.0;
    }
    let exponent = raw.log10().floor();
    let scale = 10_f64.powf(exponent);
    let fraction = raw / scale;
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_fraction * scale
}

fn round_to_step(value: f64, step: f64) -> f64 {
    if step <= 0.0 || !step.is_finite() {
        return value;
    }
    (value / step).round() * step
}

fn draw_clipped_text(buf: &mut Buffer, x: u16, y: u16, max_width: u16, text: &str, color: Color) {
    if max_width == 0 {
        return;
    }
    for (i, ch) in text.chars().take(max_width as usize).enumerate() {
        buf[(x + i as u16, y)]
            .set_symbol(ch.encode_utf8(&mut [0; 4]))
            .set_style(Style::default().fg(color));
    }
}
