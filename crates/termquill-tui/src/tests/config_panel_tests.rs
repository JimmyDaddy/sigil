use super::*;

#[test]
fn config_section_flow_wraps() {
    assert_eq!(
        ConfigSection::Provider.next_flow(),
        ConfigSection::Permissions
    );
    assert_eq!(ConfigSection::Mcp.next_flow(), ConfigSection::Provider);
    assert_eq!(ConfigSection::Provider.previous_flow(), ConfigSection::Mcp);
}

#[test]
fn config_footer_action_navigation_wraps() {
    assert_eq!(
        ConfigFooterAction::Save.next(),
        ConfigFooterAction::SaveAndClose
    );
    assert_eq!(ConfigFooterAction::Close.next(), ConfigFooterAction::Save);
    assert_eq!(
        ConfigFooterAction::Save.previous(),
        ConfigFooterAction::Close
    );
}
