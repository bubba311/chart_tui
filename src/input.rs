use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum UserAction {
    Quit,
    NextPane,
    SetLayoutSingle,
    SetLayoutTwo,
    SetLayoutQuad,
    ToggleOrderBook,
    PrevTicker,
    NextTicker,
    PrevTimeframe,
    NextTimeframe,
    PanLeft,
    PanRight,
    ZoomIn,
    ZoomOut,
    TickerBackspace,
    TickerSubmit,
    TickerCancel,
    RawChar(char),
}

pub fn poll_action(timeout: Duration) -> std::io::Result<Option<UserAction>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }

    let action = match event::read()? {
        Event::Key(KeyEvent { code, .. }) => match code {
            KeyCode::Tab => Some(UserAction::NextPane),
            KeyCode::Left => Some(UserAction::PanLeft),
            KeyCode::Right => Some(UserAction::PanRight),
            KeyCode::Up => Some(UserAction::ZoomIn),
            KeyCode::Down => Some(UserAction::ZoomOut),
            KeyCode::Char(c) => Some(UserAction::RawChar(c)),
            KeyCode::Backspace => Some(UserAction::TickerBackspace),
            KeyCode::Enter => Some(UserAction::TickerSubmit),
            KeyCode::Esc => Some(UserAction::TickerCancel),
            _ => None,
        },
        _ => None,
    };

    Ok(action)
}
