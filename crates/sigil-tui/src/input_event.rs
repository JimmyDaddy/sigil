//! Terminal-independent input facts and the Crossterm boundary adapter.

use crossterm::event::{
    Event as CrosstermEvent, KeyCode as CrosstermKeyCode, KeyEvent, KeyEventKind,
    KeyModifiers as CrosstermKeyModifiers, MouseEvent,
};

use crate::{
    app::AppAction,
    damage::Damage,
    mouse::{AppMouseOutcome, MouseInput, MouseInputKind},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Modifiers(u8);

impl Modifiers {
    #[cfg(test)]
    pub(crate) const NONE: Self = Self(0);
    #[cfg(not(test))]
    pub(crate) const CONTROL: Self = Self(Self::CONTROL_BIT);
    const SHIFT: u8 = 1 << 0;
    const CONTROL_BIT: u8 = 1 << 1;
    const ALT: u8 = 1 << 2;
    const SUPER: u8 = 1 << 3;
    const HYPER: u8 = 1 << 4;
    const META: u8 = 1 << 5;

    fn from_crossterm(modifiers: CrosstermKeyModifiers) -> Self {
        let mut bits = 0;
        if modifiers.contains(CrosstermKeyModifiers::SHIFT) {
            bits |= Self::SHIFT;
        }
        if modifiers.contains(CrosstermKeyModifiers::CONTROL) {
            bits |= Self::CONTROL_BIT;
        }
        if modifiers.contains(CrosstermKeyModifiers::ALT) {
            bits |= Self::ALT;
        }
        if modifiers.contains(CrosstermKeyModifiers::SUPER) {
            bits |= Self::SUPER;
        }
        if modifiers.contains(CrosstermKeyModifiers::HYPER) {
            bits |= Self::HYPER;
        }
        if modifiers.contains(CrosstermKeyModifiers::META) {
            bits |= Self::META;
        }
        Self(bits)
    }

    fn to_crossterm(self) -> CrosstermKeyModifiers {
        let mut modifiers = CrosstermKeyModifiers::NONE;
        if self.0 & Self::SHIFT != 0 {
            modifiers |= CrosstermKeyModifiers::SHIFT;
        }
        if self.0 & Self::CONTROL_BIT != 0 {
            modifiers |= CrosstermKeyModifiers::CONTROL;
        }
        if self.0 & Self::ALT != 0 {
            modifiers |= CrosstermKeyModifiers::ALT;
        }
        if self.0 & Self::SUPER != 0 {
            modifiers |= CrosstermKeyModifiers::SUPER;
        }
        if self.0 & Self::HYPER != 0 {
            modifiers |= CrosstermKeyModifiers::HYPER;
        }
        if self.0 & Self::META != 0 {
            modifiers |= CrosstermKeyModifiers::META;
        }
        modifiers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Esc,
    F(u8),
    Char(char),
    Unsupported,
}

impl From<CrosstermKeyCode> for InputKeyCode {
    fn from(code: CrosstermKeyCode) -> Self {
        match code {
            CrosstermKeyCode::Backspace => Self::Backspace,
            CrosstermKeyCode::Enter => Self::Enter,
            CrosstermKeyCode::Left => Self::Left,
            CrosstermKeyCode::Right => Self::Right,
            CrosstermKeyCode::Up => Self::Up,
            CrosstermKeyCode::Down => Self::Down,
            CrosstermKeyCode::Home => Self::Home,
            CrosstermKeyCode::End => Self::End,
            CrosstermKeyCode::PageUp => Self::PageUp,
            CrosstermKeyCode::PageDown => Self::PageDown,
            CrosstermKeyCode::Tab => Self::Tab,
            CrosstermKeyCode::BackTab => Self::BackTab,
            CrosstermKeyCode::Delete => Self::Delete,
            CrosstermKeyCode::Esc => Self::Esc,
            CrosstermKeyCode::F(number) => Self::F(number),
            CrosstermKeyCode::Char(character) => Self::Char(character),
            _ => Self::Unsupported,
        }
    }
}

impl From<InputKeyCode> for CrosstermKeyCode {
    fn from(code: InputKeyCode) -> Self {
        match code {
            InputKeyCode::Backspace => Self::Backspace,
            InputKeyCode::Enter => Self::Enter,
            InputKeyCode::Left => Self::Left,
            InputKeyCode::Right => Self::Right,
            InputKeyCode::Up => Self::Up,
            InputKeyCode::Down => Self::Down,
            InputKeyCode::Home => Self::Home,
            InputKeyCode::End => Self::End,
            InputKeyCode::PageUp => Self::PageUp,
            InputKeyCode::PageDown => Self::PageDown,
            InputKeyCode::Tab => Self::Tab,
            InputKeyCode::BackTab => Self::BackTab,
            InputKeyCode::Delete => Self::Delete,
            InputKeyCode::Esc => Self::Esc,
            InputKeyCode::F(number) => Self::F(number),
            InputKeyCode::Char(character) => Self::Char(character),
            InputKeyCode::Unsupported => Self::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKeyEventKind {
    Press,
    Repeat,
    Release,
}

impl From<KeyEventKind> for InputKeyEventKind {
    fn from(kind: KeyEventKind) -> Self {
        match kind {
            KeyEventKind::Press => Self::Press,
            KeyEventKind::Repeat => Self::Repeat,
            KeyEventKind::Release => Self::Release,
        }
    }
}

impl From<InputKeyEventKind> for KeyEventKind {
    fn from(kind: InputKeyEventKind) -> Self {
        match kind {
            InputKeyEventKind::Press => Self::Press,
            InputKeyEventKind::Repeat => Self::Repeat,
            InputKeyEventKind::Release => Self::Release,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputKeyEvent {
    pub(crate) code: InputKeyCode,
    pub(crate) modifiers: Modifiers,
    pub(crate) kind: InputKeyEventKind,
}

impl From<KeyEvent> for InputKeyEvent {
    fn from(event: KeyEvent) -> Self {
        Self {
            code: event.code.into(),
            modifiers: Modifiers::from_crossterm(event.modifiers),
            kind: event.kind.into(),
        }
    }
}

impl InputKeyEvent {
    #[cfg(not(test))]
    pub(crate) fn to_crossterm(self) -> KeyEvent {
        KeyEvent::new_with_kind(
            self.code.into(),
            self.modifiers.to_crossterm(),
            self.kind.into(),
        )
    }

    pub(crate) const fn may_change_state(self) -> bool {
        !matches!(self.code, InputKeyCode::Unsupported)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputMouseEvent {
    pub(crate) column: u16,
    pub(crate) row: u16,
    pub(crate) kind: MouseInputKind,
    pub(crate) modifiers: Modifiers,
}

impl From<MouseEvent> for InputMouseEvent {
    fn from(event: MouseEvent) -> Self {
        Self {
            column: event.column,
            row: event.row,
            kind: event.kind.into(),
            modifiers: Modifiers::from_crossterm(event.modifiers),
        }
    }
}

impl From<InputMouseEvent> for MouseInput {
    fn from(event: InputMouseEvent) -> Self {
        Self {
            column: event.column,
            row: event.row,
            kind: event.kind,
            modifiers: event.modifiers.to_crossterm(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusChange {
    Gained,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputEvent {
    Key(InputKeyEvent),
    Mouse(InputMouseEvent),
    Paste(String),
    Resize { width: u16, height: u16 },
    Focus(FocusChange),
}

impl From<CrosstermEvent> for InputEvent {
    fn from(event: CrosstermEvent) -> Self {
        match event {
            CrosstermEvent::Key(key) => Self::Key(key.into()),
            CrosstermEvent::Mouse(mouse) => Self::Mouse(mouse.into()),
            CrosstermEvent::Paste(text) => Self::Paste(text),
            CrosstermEvent::Resize(width, height) => Self::Resize { width, height },
            CrosstermEvent::FocusGained => Self::Focus(FocusChange::Gained),
            CrosstermEvent::FocusLost => Self::Focus(FocusChange::Lost),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum HostRequest {
    CopyText { text: String, secret: bool },
    OpenExternalUrl { url: String, secret: bool },
    RevealFile(std::path::PathBuf),
}

#[derive(Debug, Clone)]
pub(crate) enum EventEffect {
    Ignored,
    LocalUpdate(Damage),
    OpaqueAction(AppAction),
    HostRequest(HostRequest),
}

impl AppAction {
    pub(crate) fn into_event_effect(self) -> EventEffect {
        match self {
            Self::CopyToClipboard { text } => EventEffect::HostRequest(HostRequest::CopyText {
                text,
                secret: false,
            }),
            Self::CopySecretToClipboard { text } => {
                EventEffect::HostRequest(HostRequest::CopyText {
                    text: text.expose_secret().to_owned(),
                    secret: true,
                })
            }
            Self::OpenExternalUrl { url } => {
                EventEffect::HostRequest(HostRequest::OpenExternalUrl { url, secret: false })
            }
            Self::OpenSecretExternalUrl { url } => {
                EventEffect::HostRequest(HostRequest::OpenExternalUrl {
                    url: url.expose_secret().to_owned(),
                    secret: true,
                })
            }
            Self::RevealFile { path } => EventEffect::HostRequest(HostRequest::RevealFile(path)),
            action => EventEffect::OpaqueAction(action),
        }
    }
}

impl AppMouseOutcome {
    pub(crate) fn into_event_effect(self) -> EventEffect {
        match self {
            Self::Noop => EventEffect::Ignored,
            Self::Redraw => EventEffect::LocalUpdate(Damage::INPUT),
            Self::Action(action) => action.into_event_effect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use crate::{app::AppAction, mouse::AppMouseOutcome};

    use super::{
        FocusChange, InputEvent, InputKeyCode, InputKeyEvent, InputKeyEventKind, Modifiers,
    };

    #[test]
    fn crossterm_key_is_normalized_without_leaking_key_event_to_the_dispatch_contract() {
        let event = InputEvent::from(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyEventKind::Press,
        )));
        assert_eq!(
            event,
            InputEvent::Key(InputKeyEvent {
                code: InputKeyCode::Char('x'),
                modifiers: Modifiers(1 << 1 | 1 << 2),
                kind: InputKeyEventKind::Press,
            })
        );
    }

    #[test]
    fn unsupported_keys_are_ignored_and_focus_is_explicit() {
        assert!(
            !InputKeyEvent {
                code: InputKeyCode::Unsupported,
                modifiers: Modifiers::NONE,
                kind: InputKeyEventKind::Press,
            }
            .may_change_state()
        );
        assert_eq!(
            InputEvent::from(Event::FocusGained),
            InputEvent::Focus(FocusChange::Gained)
        );
    }

    #[test]
    fn action_and_mouse_outcomes_are_split_into_local_opaque_and_host_effects() {
        assert!(matches!(
            AppMouseOutcome::Noop.into_event_effect(),
            super::EventEffect::Ignored
        ));
        assert!(matches!(
            AppMouseOutcome::Redraw.into_event_effect(),
            super::EventEffect::LocalUpdate(damage) if !damage.is_empty()
        ));
        assert!(matches!(
            AppAction::OpenExternalUrl {
                url: "https://example.com".to_owned(),
            }
            .into_event_effect(),
            super::EventEffect::HostRequest(super::HostRequest::OpenExternalUrl {
                secret: false,
                ..
            })
        ));
        assert!(matches!(
            AppAction::CancelRun.into_event_effect(),
            super::EventEffect::OpaqueAction(AppAction::CancelRun)
        ));
    }
}
