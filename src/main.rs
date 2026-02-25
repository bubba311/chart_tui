mod app;
mod data;
mod feed;
mod input;
mod perf;
mod providers;
mod render;

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use app::{App, TARGET_FRAME_TIME};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use perf::RuntimeStats;
use providers::schwab::streamer::SchwabProvider;
use providers::schwab::{
    default_token_file_path, extract_auth_code_and_state, load_tokens, save_tokens, SchwabOAuth,
    SchwabOAuthConfig, StoredTokens,
};
use providers::synthetic::SyntheticProvider;
use providers::{MarketDataProvider, OAuthProvider};
use ratatui::{backend::CrosstermBackend, Terminal};

const MOCK_FEED_INTERVAL_MS: u64 = 80;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match parse_args(std::env::args().collect())? {
        Command::RunTui => run_tui(),
        Command::SchwabAuthUrl { state, out } => run_schwab_auth_url(&state, out.as_deref()),
        Command::SchwabLogin { state, token_file } => run_schwab_login(&state, token_file),
        Command::SchwabRefresh { token_file } => run_schwab_refresh(token_file),
    }
}

fn run_tui() -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal_guard = TerminalGuard::setup()?;
    app_loop(terminal_guard.terminal_mut())
}

fn run_schwab_auth_url(state: &str, out: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = SchwabOAuthConfig::from_env().map_err(std::io::Error::other)?;
    let oauth = SchwabOAuth::new(cfg);
    let auth_url = oauth
        .authorization_url(state)
        .map_err(std::io::Error::other)?;
    let token_url = oauth.token_url();

    let output = format!(
        "Schwab OAuth Authorization URL\n{}\n\nToken URL\n{}\n",
        auth_url, token_url
    );

    if let Some(path) = out {
        fs::write(path, &output)?;
        println!("Wrote Schwab OAuth URLs to {}", path);
    } else {
        println!("{}", output);
    }
    Ok(())
}

fn run_schwab_login(
    state: &str,
    token_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = SchwabOAuthConfig::from_env().map_err(std::io::Error::other)?;
    let oauth = SchwabOAuth::new(cfg);
    let auth_url = oauth
        .authorization_url(state)
        .map_err(std::io::Error::other)?;
    let target = token_file.unwrap_or_else(default_token_file_path);

    println!(
        "Open this Schwab OAuth URL in your browser:\n\n{}\n",
        auth_url
    );
    println!("After approving, paste the full callback URL (or just the code):");
    print!("> ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let (code, returned_state) =
        extract_auth_code_and_state(&input).map_err(std::io::Error::other)?;
    if let Some(received) = returned_state {
        if received != state {
            return Err(format!(
                "state mismatch: expected '{}', received '{}'",
                state, received
            )
            .into());
        }
    }

    let token = oauth
        .exchange_authorization_code(&code)
        .map_err(std::io::Error::other)?;
    let stored = StoredTokens::from_response(token);
    save_tokens(&target, &stored).map_err(std::io::Error::other)?;
    println!("Saved Schwab tokens to {}", target.display());
    Ok(())
}

fn run_schwab_refresh(token_file: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = SchwabOAuthConfig::from_env().map_err(std::io::Error::other)?;
    let oauth = SchwabOAuth::new(cfg);
    let target = token_file.unwrap_or_else(default_token_file_path);
    let current = load_tokens(&target).map_err(std::io::Error::other)?;
    let refresh_token = current
        .refresh_token
        .as_deref()
        .ok_or("no refresh_token found in stored token file")?;

    let token = oauth
        .refresh_access_token(refresh_token)
        .map_err(std::io::Error::other)?;
    let mut updated = StoredTokens::from_response(token);
    if updated.refresh_token.is_none() {
        updated.refresh_token = Some(refresh_token.to_string());
    }
    save_tokens(&target, &updated).map_err(std::io::Error::other)?;
    println!("Refreshed Schwab access token in {}", target.display());
    Ok(())
}

#[derive(Debug, Clone)]
enum Command {
    RunTui,
    SchwabAuthUrl {
        state: String,
        out: Option<String>,
    },
    SchwabLogin {
        state: String,
        token_file: Option<PathBuf>,
    },
    SchwabRefresh {
        token_file: Option<PathBuf>,
    },
}

fn parse_args(args: Vec<String>) -> Result<Command, Box<dyn std::error::Error>> {
    if args.len() <= 1 {
        return Ok(Command::RunTui);
    }

    match args[1].as_str() {
        "schwab-auth-url" => {
            let mut state = "chart_tui_state".to_string();
            let mut out: Option<String> = None;

            let mut i = 2_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--state" => {
                        i += 1;
                        if i >= args.len() {
                            return Err("--state requires a value".into());
                        }
                        state = args[i].clone();
                    }
                    "--out" => {
                        i += 1;
                        if i >= args.len() {
                            return Err("--out requires a value".into());
                        }
                        out = Some(args[i].clone());
                    }
                    other => {
                        return Err(format!(
                            "unknown argument '{}'. expected --state <value> and/or --out <path>",
                            other
                        )
                        .into())
                    }
                }
                i += 1;
            }
            Ok(Command::SchwabAuthUrl { state, out })
        }
        "schwab-login" => {
            let mut state = "chart_tui_state".to_string();
            let mut token_file: Option<PathBuf> = None;

            let mut i = 2_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--state" => {
                        i += 1;
                        if i >= args.len() {
                            return Err("--state requires a value".into());
                        }
                        state = args[i].clone();
                    }
                    "--token-file" => {
                        i += 1;
                        if i >= args.len() {
                            return Err("--token-file requires a value".into());
                        }
                        token_file = Some(PathBuf::from(args[i].clone()));
                    }
                    other => {
                        return Err(format!(
                            "unknown argument '{}'. expected --state <value> and/or --token-file <path>",
                            other
                        )
                        .into())
                    }
                }
                i += 1;
            }
            Ok(Command::SchwabLogin { state, token_file })
        }
        "schwab-refresh-token" => {
            let mut token_file: Option<PathBuf> = None;
            let mut i = 2_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--token-file" => {
                        i += 1;
                        if i >= args.len() {
                            return Err("--token-file requires a value".into());
                        }
                        token_file = Some(PathBuf::from(args[i].clone()));
                    }
                    other => {
                        return Err(format!(
                            "unknown argument '{}'. expected --token-file <path>",
                            other
                        )
                        .into())
                    }
                }
                i += 1;
            }
            Ok(Command::SchwabRefresh { token_file })
        }
        other => Err(format!(
            "unknown command '{}'. supported: 'schwab-auth-url', 'schwab-login', 'schwab-refresh-token'",
            other
        )
        .into()),
    }
}

fn app_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    let mut stats = RuntimeStats::new();
    let provider_kind =
        std::env::var("CHART_TUI_PROVIDER").unwrap_or_else(|_| "schwab".to_string());
    let mut provider: Box<dyn MarketDataProvider> = match provider_kind.as_str() {
        "schwab" => Box::new(SchwabProvider::new(Duration::from_millis(
            MOCK_FEED_INTERVAL_MS,
        ))),
        _ => Box::new(SyntheticProvider::new(Duration::from_millis(
            MOCK_FEED_INTERVAL_MS,
        ))),
    };
    let mut subscribed_symbols = app
        .panes
        .iter()
        .map(|p| p.symbol.clone())
        .collect::<Vec<_>>();
    provider.subscribe(subscribed_symbols.clone());
    app.status_message = Some(format!("Provider: {}", provider.name()));
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

        let current_symbols = app
            .panes
            .iter()
            .map(|p| p.symbol.clone())
            .collect::<Vec<_>>();
        if current_symbols != subscribed_symbols {
            subscribed_symbols = current_symbols;
            provider.subscribe(subscribed_symbols.clone());
        }
        let feed_events = app.ingest_market_events(provider.poll_events());

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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{parse_args, Command};

    #[test]
    fn parses_schwab_auth_url_command() {
        let cmd = parse_args(vec![
            "chart_tui".to_string(),
            "schwab-auth-url".to_string(),
            "--state".to_string(),
            "abc".to_string(),
            "--out".to_string(),
            "auth.txt".to_string(),
        ])
        .expect("parse args");

        match cmd {
            Command::SchwabAuthUrl { state, out } => {
                assert_eq!(state, "abc");
                assert_eq!(out.as_deref(), Some("auth.txt"));
            }
            _ => panic!("expected SchwabAuthUrl command"),
        }
    }

    #[test]
    fn parses_schwab_login_command() {
        let cmd = parse_args(vec![
            "chart_tui".to_string(),
            "schwab-login".to_string(),
            "--state".to_string(),
            "abc".to_string(),
            "--token-file".to_string(),
            "/tmp/schwab.json".to_string(),
        ])
        .expect("parse args");

        match cmd {
            Command::SchwabLogin { state, token_file } => {
                assert_eq!(state, "abc");
                assert_eq!(token_file, Some(PathBuf::from("/tmp/schwab.json")));
            }
            _ => panic!("expected SchwabLogin command"),
        }
    }

    #[test]
    fn parses_schwab_refresh_command() {
        let cmd = parse_args(vec![
            "chart_tui".to_string(),
            "schwab-refresh-token".to_string(),
            "--token-file".to_string(),
            "/tmp/schwab.json".to_string(),
        ])
        .expect("parse args");

        match cmd {
            Command::SchwabRefresh { token_file } => {
                assert_eq!(token_file, Some(PathBuf::from("/tmp/schwab.json")));
            }
            _ => panic!("expected SchwabRefresh command"),
        }
    }
}
