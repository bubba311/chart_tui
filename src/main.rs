mod app;
mod data;
mod feed;
mod input;
mod perf;
mod render;

use std::time::Duration;
use std::time::Instant;

use app::{App, TARGET_FRAME_TIME};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use perf::RuntimeStats;
use ratatui::{backend::CrosstermBackend, Terminal};

const MOCK_FEED_INTERVAL_MS: u64 = 80;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run()
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal_guard = TerminalGuard::setup()?;
    app_loop(terminal_guard.terminal_mut())
}

fn app_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    let mut stats = RuntimeStats::new();
    let feed_rx = feed::start_mock_feed(
        app.panes.len(),
        Duration::from_millis(MOCK_FEED_INTERVAL_MS),
    );
    let mut next_frame_at = Instant::now();

    while !app.should_quit {
        let frame_start = Instant::now();
        let now = Instant::now();
        let input_timeout = next_frame_at
            .saturating_duration_since(now)
            .min(Duration::from_millis(2));
        let update_start = Instant::now();
        if let Some(action) = input::poll_action(input_timeout)? {
            app.handle_action(action);
        }

        let feed_events = app.drain_feed(&feed_rx);

        let now = Instant::now();
        if now >= next_frame_at {
            app.refresh_dirty_stats();
            let update_elapsed = update_start.elapsed();
            let snapshot = stats.snapshot();
            let render_start = Instant::now();
            terminal.draw(|frame| render::draw(frame, &app, &snapshot))?;
            let render_elapsed = render_start.elapsed();
            let frame_elapsed = frame_start.elapsed();

            stats.record_frame(update_elapsed, render_elapsed, frame_elapsed, feed_events);

            next_frame_at += TARGET_FRAME_TIME;
            while next_frame_at <= now {
                next_frame_at += TARGET_FRAME_TIME;
            }
        } else {
            std::thread::sleep(
                next_frame_at
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(1)),
            );
        }
    }

    Ok(())
}

fn setup_terminal(
) -> Result<Terminal<CrosstermBackend<std::io::Stdout>>, Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

struct TerminalGuard {
    terminal: Option<Terminal<CrosstermBackend<std::io::Stdout>>>,
}

impl TerminalGuard {
    fn setup() -> Result<Self, Box<dyn std::error::Error>> {
        let terminal = setup_terminal()?;
        Ok(Self {
            terminal: Some(terminal),
        })
    }

    fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<std::io::Stdout>> {
        self.terminal
            .as_mut()
            .expect("terminal is always set while app is running")
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.as_mut() {
            let _ = restore_terminal(terminal);
        }
    }
}
