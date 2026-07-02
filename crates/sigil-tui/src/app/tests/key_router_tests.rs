use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{InputContext, RoutedKeyCommand, key_binding_snapshot, resolve_binding};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn key_router_maps_activity_agent_list_keys_without_sidebar_fallback() {
    assert_eq!(
        resolve_binding(InputContext::ActivityAgentList, key(KeyCode::Down)),
        Some(RoutedKeyCommand::ActivityAgentNext)
    );
    assert_eq!(
        resolve_binding(InputContext::ActivityAgentList, key(KeyCode::Up)),
        Some(RoutedKeyCommand::ActivityAgentPrevious)
    );
    assert_eq!(
        resolve_binding(InputContext::ActivityAgentList, key(KeyCode::Enter)),
        Some(RoutedKeyCommand::ActivityAgentActivate)
    );
    assert_eq!(
        resolve_binding(
            InputContext::ActivityAgentList,
            KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL),
        ),
        None
    );
}

#[test]
fn key_router_maps_approval_keys_before_legacy_fallbacks() {
    assert_eq!(
        resolve_binding(InputContext::ApprovalModal, key(KeyCode::Enter)),
        Some(RoutedKeyCommand::ApprovalSelect)
    );
    assert_eq!(
        resolve_binding(InputContext::ApprovalModal, key(KeyCode::Tab)),
        Some(RoutedKeyCommand::ApprovalActionNext)
    );
    assert_eq!(
        resolve_binding(InputContext::ApprovalModal, key(KeyCode::Down)),
        Some(RoutedKeyCommand::ApprovalScrollDown)
    );
    assert_eq!(
        resolve_binding(InputContext::ApprovalModal, key(KeyCode::Char('v'))),
        Some(RoutedKeyCommand::ApprovalDiffMode)
    );
    assert_eq!(
        resolve_binding(
            InputContext::ApprovalModal,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        ),
        None
    );
}

#[test]
fn key_router_keeps_agent_panel_letters_available_for_composer_text() {
    assert_eq!(
        resolve_binding(InputContext::ComposerAgentPanel, key(KeyCode::Char('c'))),
        None
    );
    assert_eq!(
        resolve_binding(InputContext::ComposerAgentPanel, key(KeyCode::Char('m'))),
        None
    );
    assert_eq!(
        resolve_binding(
            InputContext::ComposerAgentPanel,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
        ),
        Some(RoutedKeyCommand::AgentClose)
    );
    assert_eq!(
        resolve_binding(
            InputContext::ComposerAgentPanel,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
        ),
        Some(RoutedKeyCommand::AgentMessage)
    );
}

#[test]
fn key_router_snapshot_covers_high_risk_contexts() {
    let snapshot = key_binding_snapshot();
    assert!(snapshot.iter().any(|binding| {
        binding.context == InputContext::ApprovalModal
            && binding.key == "Enter"
            && binding.command == RoutedKeyCommand::ApprovalSelect
    }));
    assert!(snapshot.iter().any(|binding| {
        binding.context == InputContext::ComposerQueuePanel
            && binding.key == "Down"
            && binding.command == RoutedKeyCommand::QueueSelectionNext
    }));
    assert!(snapshot.iter().any(|binding| {
        binding.context == InputContext::ComposerAgentPanel
            && binding.key == "Down"
            && binding.command == RoutedKeyCommand::AgentSelectionNext
    }));
    assert!(snapshot.iter().any(|binding| {
        binding.context == InputContext::ActivityAgentList
            && binding.key == "Down"
            && binding.command == RoutedKeyCommand::ActivityAgentNext
    }));
}
