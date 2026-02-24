use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAction {
    Quit,
    NextPane,
    SetLayoutSingle,
    SetLayoutTwo,
    SetLayoutQuad,
    PrevTimeframe,
    NextTimeframe,
    PanLeft,
    PanRight,
    ZoomIn,
    ZoomOut,
}

pub fn poll_action(timeout: Duration) -> std::io::Result<Option<UserAction>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }

    let action = match event::read()? {
        Event::Key(KeyEvent { code, .. }) => match code {
            KeyCode::Char('q') => Some(UserAction::Quit),
            KeyCode::Tab => Some(UserAction::NextPane),
            KeyCode::Char('1') => Some(UserAction::SetLayoutSingle),
            KeyCode::Char('2') => Some(UserAction::SetLayoutTwo),
            KeyCode::Char('4') => Some(UserAction::SetLayoutQuad),
            KeyCode::Char('[') => Some(UserAction::PrevTimeframe),
            KeyCode::Char(']') => Some(UserAction::NextTimeframe),
            KeyCode::Char('+') | KeyCode::Char('=') => Some(UserAction::ZoomIn),
            KeyCode::Char('-') | KeyCode::Char('_') => Some(UserAction::ZoomOut),
            KeyCode::Left => Some(UserAction::PanLeft),
            KeyCode::Right => Some(UserAction::PanRight),
            KeyCode::Up => Some(UserAction::ZoomIn),
            KeyCode::Down => Some(UserAction::ZoomOut),
            _ => None,
        },
        _ => None,
    };

    Ok(action)
}
