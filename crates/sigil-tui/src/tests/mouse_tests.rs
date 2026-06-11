use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{MouseInput, MouseInputKind};

#[test]
fn mouse_event_kind_maps_supported_events() {
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Down(MouseButton::Left)),
        MouseInputKind::LeftDown
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Up(MouseButton::Left)),
        MouseInputKind::LeftUp
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Down(MouseButton::Right)),
        MouseInputKind::RightDown
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::ScrollUp),
        MouseInputKind::ScrollUp
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::ScrollDown),
        MouseInputKind::ScrollDown
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Drag(MouseButton::Left)),
        MouseInputKind::Drag
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Moved),
        MouseInputKind::Moved
    );
}

#[test]
fn mouse_event_kind_maps_unknown_buttons_to_unsupported() {
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Down(MouseButton::Middle)),
        MouseInputKind::Unsupported
    );
    assert_eq!(
        MouseInputKind::from(MouseEventKind::Up(MouseButton::Right)),
        MouseInputKind::Unsupported
    );
}

#[test]
fn mouse_event_conversion_preserves_coordinates_and_modifiers() {
    let event = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 12,
        row: 7,
        modifiers: KeyModifiers::CONTROL,
    };

    assert_eq!(
        MouseInput::from(event),
        MouseInput {
            column: 12,
            row: 7,
            kind: MouseInputKind::ScrollDown,
            modifiers: KeyModifiers::CONTROL,
        }
    );
}
