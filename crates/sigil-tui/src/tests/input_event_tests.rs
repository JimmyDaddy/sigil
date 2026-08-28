use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{app::AppAction, mouse::AppMouseOutcome};

use super::{FocusChange, InputEvent, InputKeyCode, InputKeyEvent, InputKeyEventKind, Modifiers};

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
        super::EventEffect::HostRequest(super::HostRequest::OpenExternalUrl { secret: false, .. })
    ));
    assert!(matches!(
        AppAction::CancelRun.into_event_effect(),
        super::EventEffect::OpaqueAction(AppAction::CancelRun)
    ));
}
