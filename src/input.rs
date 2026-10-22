use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

pub enum Action {
    Quit,
    Jump,
    AddBall,
    RemoveBall,
    Pause,
    Palette,
    Sparks,
    Stars,
    Help,
    Reset,
    Trails,
    Resize(u16, u16),
}

pub struct Input {
    left_until: Instant,
    right_until: Instant,
}
impl Input {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            left_until: now,
            right_until: now,
        }
    }
    pub fn steer(&self, now: Instant) -> f32 {
        (now < self.right_until) as u8 as f32 - (now < self.left_until) as u8 as f32
    }
    pub fn handle(&mut self, event: Event, now: Instant) -> Option<Action> {
        let key = match event {
            Event::Resize(w, h) => return Some(Action::Resize(w, h)),
            Event::Key(key) => key,
            _ => return None,
        };
        let code = match key.code {
            KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
            other => other,
        };
        if matches!(
            code,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('a' | 'd')
        ) {
            // Most terminals do not send release events. A short, renewed steering
            // pulse supports autorepeat without leaving a direction stuck on.
            let until = if key.kind == KeyEventKind::Release {
                now
            } else {
                now + Duration::from_millis(180)
            };
            if matches!(code, KeyCode::Left | KeyCode::Char('a')) {
                self.left_until = until;
            } else {
                self.right_until = until;
            }
            return None;
        }
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Action::Quit);
        }
        if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
            return Some(Action::Quit);
        }
        // Enhanced terminals distinguish repeats; do not repeatedly flip toggles.
        if key.kind == KeyEventKind::Repeat {
            return None;
        }
        match code {
            KeyCode::Char(' ') => Some(Action::Jump),
            KeyCode::Char('+' | '=') => Some(Action::AddBall),
            KeyCode::Char('-') => Some(Action::RemoveBall),
            KeyCode::Char('p') => Some(Action::Pause),
            KeyCode::Char('t') => Some(Action::Palette),
            KeyCode::Char('s') => Some(Action::Sparks),
            KeyCode::Char('b') => Some(Action::Stars),
            KeyCode::Char('h') => Some(Action::Help),
            KeyCode::Char('r') => Some(Action::Reset),
            KeyCode::Char('l') => Some(Action::Trails),
            _ => None,
        }
    }
}
